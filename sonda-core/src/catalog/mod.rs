//! Catalog directory enumeration and `@name` resolution.
//!
//! Two sources feed the catalog: the packs [`builtin`] embeds in the binary,
//! and a user directory passed as `--catalog <dir>`. [`CatalogPackResolver`]
//! chains them — user directory first, builtins behind it — so every surface
//! that already builds one (the CLI, the server's `POST /scenarios`, and
//! `--autostart`) gets the builtins with no wiring of its own.

pub mod builtin;

use std::fs;
use std::path::{Path, PathBuf};

use crate::compiler::expand::{
    classify_pack_reference, PackResolveError, PackResolveOrigin, PackResolver,
};
use crate::packs::MetricPackDef;

/// Errors from catalog directory enumeration and `@name` resolution.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("catalog dir {dir} does not exist or is not a directory")]
    NotADirectory { dir: String },

    #[error("failed to read catalog dir {dir}")]
    ReadDir {
        dir: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to read entry in {dir}")]
    ReadEntry {
        dir: String,
        #[source]
        source: std::io::Error,
    },

    /// Currently unconstructed: enumeration warns and skips instead. Kept for API stability.
    #[error("failed to read {path}")]
    ReadFile {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("cannot derive name from filename {path}")]
    InvalidName { path: String },

    #[error("catalog {dir} contains duplicate entry name {name:?}: {first} and {second}")]
    DuplicateName {
        dir: String,
        name: String,
        first: String,
        second: String,
    },

    #[error("unknown catalog entry {name:?} in {dir}; available: {available}")]
    UnknownEntry {
        dir: String,
        name: String,
        available: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Runnable,
    Composable,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EntryKind::Runnable => "runnable",
            EntryKind::Composable => "composable",
        }
    }
}

/// Where a [`CatalogEntry`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryOrigin {
    /// A YAML file in the `--catalog <dir>` directory.
    UserDir,
    /// Embedded in the binary by [`builtin`].
    Builtin,
}

#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub name: String,
    pub kind: EntryKind,
    pub description: String,
    /// The pack's `category:`, which `sonda list` groups by. Runnable
    /// scenarios generally do not declare one.
    pub category: Option<String>,
    pub tags: Vec<String>,
    /// For a builtin this is the un-openable `<builtin>/<file>` marker
    /// [`builtin::source_path`] produces, not a path on disk.
    pub source_path: PathBuf,
    pub origin: EntryOrigin,
    /// Set only by [`merged`]: this user entry hides a builtin of the same
    /// name. Always `false` from [`enumerate`].
    pub shadows_builtin: bool,
    /// Why this `composable` entry cannot be referenced by `pack: <name>`,
    /// rendered — either it does not deserialize as a
    /// [`MetricPackDef`](crate::packs::MetricPackDef), or it does and fails
    /// [`validate_pack`](crate::packs::validate_pack).
    ///
    /// Such an entry is still listed and still readable with `sonda show`,
    /// which is how a user finds what is wrong with it. Always `None` for a
    /// `runnable` entry — the compiler is the judge of those.
    pub pack_error: Option<String>,
}

/// Why a YAML file did not become a [`CatalogEntry`].
///
/// `enumerate` drops these files — historically in silence, which reads as
/// coverage. [`enumerate_with_skips`] returns them so a caller can say which
/// files it ignored and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// No `kind:` key. The common case: a data file sharing the directory.
    MissingKind,
    /// `kind:` present but neither `runnable` nor `composable`.
    UnknownKind(String),
    /// The file could not be read.
    Unreadable(String),
    /// Not valid YAML, or a header key with the wrong type.
    Unparseable(String),
}

impl SkipReason {
    /// One clause naming the reason, for the note `sonda list` prints.
    pub fn describe(&self) -> String {
        match self {
            SkipReason::MissingKind => "no `kind:` header".to_string(),
            SkipReason::UnknownKind(k) => {
                format!("kind: {k:?} is neither 'runnable' nor 'composable'")
            }
            SkipReason::Unreadable(e) => format!("unreadable: {e}"),
            SkipReason::Unparseable(e) => format!("unparseable YAML: {e}"),
        }
    }

    /// Whether this skip is a malfunction rather than an ordinary
    /// non-catalog file. [`enumerate`] warns on stderr for these.
    fn is_malfunction(&self) -> bool {
        matches!(self, SkipReason::Unreadable(_) | SkipReason::Unparseable(_))
    }
}

/// A YAML file the walk passed over, with the reason.
#[derive(Debug, Clone)]
pub struct SkippedFile {
    pub path: PathBuf,
    pub reason: SkipReason,
}

/// The result of a catalog walk: what it found, and what it passed over.
#[derive(Debug, Clone, Default)]
pub struct CatalogListing {
    pub entries: Vec<CatalogEntry>,
    pub skipped: Vec<SkippedFile>,
}

#[derive(serde::Deserialize)]
struct CatalogEntryHeader {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    scenario_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

/// Classify one YAML document's header into a catalog entry.
///
/// The single definition of what counts as a catalog entry: both the
/// on-disk walk and [`builtin::entries`] go through here, so a builtin and a
/// user file with identical headers produce identical rows.
///
/// `fallback_name` is used when the document declares neither
/// `scenario_name:` nor `name:` — the caller supplies the filename stem.
pub(crate) fn header_entry(
    content: &str,
    fallback_name: &str,
    source_path: PathBuf,
    origin: EntryOrigin,
) -> Result<CatalogEntry, SkipReason> {
    let header: CatalogEntryHeader =
        serde_yaml_ng::from_str(content).map_err(|e| SkipReason::Unparseable(e.to_string()))?;
    let raw_kind = header.kind.ok_or(SkipReason::MissingKind)?;
    let kind = match raw_kind.as_str() {
        "runnable" => EntryKind::Runnable,
        "composable" => EntryKind::Composable,
        other => return Err(SkipReason::UnknownKind(other.to_string())),
    };
    let name = header
        .scenario_name
        .or(header.name)
        .unwrap_or_else(|| fallback_name.replace('_', "-"));
    // A composable entry that cannot be referenced is still an entry: it is
    // listed, marked, and readable with `sonda show`. Skipping it would hide
    // the file from the one command that shows what is wrong with it.
    let pack_error = match kind {
        EntryKind::Composable => pack_error(content),
        EntryKind::Runnable => None,
    };
    Ok(CatalogEntry {
        name,
        kind,
        description: header.description.unwrap_or_default(),
        category: header.category,
        tags: header.tags,
        source_path,
        origin,
        shadows_builtin: false,
        pack_error,
    })
}

/// Why `content` cannot serve as a pack, or `None` if it can.
///
/// Runs the same two steps the resolver does before expansion, so a
/// listing's verdict and a run's cannot disagree.
fn pack_error(content: &str) -> Option<String> {
    match serde_yaml_ng::from_str::<crate::packs::MetricPackDef>(content) {
        Ok(pack) => crate::packs::validate_pack(&pack)
            .err()
            .map(|e| e.to_string()),
        Err(e) => Some(e.to_string()),
    }
}

/// Walk `dir` and return one [`CatalogEntry`] per YAML file with a
/// recognized `kind:` header, plus every YAML file that was passed over and
/// why. The walk is flat — one [`fs::read_dir`], no recursion — so a pack
/// belongs directly in the catalog directory and is grouped by its
/// `category:` rather than by a subdirectory.
///
/// A file that cannot be read or parsed is skipped rather than fatal, so one
/// bad file never costs the rest of the catalog. This function reports and
/// prints nothing; [`enumerate`] is the variant that warns on stderr.
pub fn enumerate_with_skips(dir: &Path) -> Result<CatalogListing, CatalogError> {
    if !dir.is_dir() {
        return Err(CatalogError::NotADirectory {
            dir: dir.display().to_string(),
        });
    }

    let mut entries: Vec<CatalogEntry> = Vec::new();
    let mut skipped: Vec<SkippedFile> = Vec::new();
    let read_dir = fs::read_dir(dir).map_err(|source| CatalogError::ReadDir {
        dir: dir.display().to_string(),
        source,
    })?;
    for entry in read_dir {
        let entry = entry.map_err(|source| CatalogError::ReadEntry {
            dir: dir.display().to_string(),
            source,
        })?;
        let path = entry.path();
        if !is_yaml_file(&path) {
            continue;
        }
        let fallback_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| CatalogError::InvalidName {
                path: path.display().to_string(),
            })?
            .to_string();
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) => {
                skipped.push(SkippedFile {
                    path: path.clone(),
                    reason: SkipReason::Unreadable(e.to_string()),
                });
                continue;
            }
        };
        match header_entry(&content, &fallback_name, path.clone(), EntryOrigin::UserDir) {
            Ok(parsed) => entries.push(parsed),
            Err(reason) => skipped.push(SkippedFile { path, reason }),
        }
    }

    entries.sort_by(|a, b| a.name.cmp(&b.name));
    skipped.sort_by(|a, b| a.path.cmp(&b.path));

    for pair in entries.windows(2) {
        if pair[0].name == pair[1].name {
            return Err(CatalogError::DuplicateName {
                dir: dir.display().to_string(),
                name: pair[0].name.clone(),
                first: pair[0].source_path.display().to_string(),
                second: pair[1].source_path.display().to_string(),
            });
        }
    }

    Ok(CatalogListing { entries, skipped })
}

/// [`enumerate_with_skips`], discarding the skip list after warning on
/// stderr about the files that were skipped because something was wrong with
/// them (unreadable, unparseable) rather than because they are not catalog
/// entries at all.
pub fn enumerate(dir: &Path) -> Result<Vec<CatalogEntry>, CatalogError> {
    let listing = enumerate_with_skips(dir)?;
    for skip in &listing.skipped {
        if skip.reason.is_malfunction() {
            eprintln!(
                "warning: catalog: skipping {}: {}",
                skip.path.display(),
                skip.reason.describe()
            );
        }
    }
    Ok(listing.entries)
}

/// The builtin catalog with a user directory merged over it.
///
/// On a name collision the user's entry wins and is flagged
/// [`CatalogEntry::shadows_builtin`]; the builtin it hides is dropped from
/// the listing rather than shown twice. With `user_dir` absent this is just
/// the builtin set, which is what `sonda list` with no arguments prints.
pub fn merged(user_dir: Option<&Path>) -> Result<CatalogListing, CatalogError> {
    let user = match user_dir {
        Some(dir) => enumerate_with_skips(dir)?,
        None => CatalogListing::default(),
    };

    let mut entries = user.entries;
    for entry in entries.iter_mut() {
        entry.shadows_builtin = builtin::find(&entry.name).is_some();
    }
    for candidate in builtin::entries() {
        if !entries.iter().any(|e| e.name == candidate.name) {
            entries.push(candidate);
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(CatalogListing {
        entries,
        skipped: user.skipped,
    })
}

/// Resolve `@name` against `dir` and return the source YAML path.
pub fn resolve(dir: &Path, name: &str) -> Result<PathBuf, CatalogError> {
    let all = enumerate(dir)?;
    if let Some(entry) = all.iter().find(|e| e.name == name) {
        return Ok(entry.source_path.clone());
    }
    let names: Vec<String> = all.iter().map(|e| e.name.clone()).collect();
    let available = if names.is_empty() {
        "<empty>".to_string()
    } else {
        names.join(", ")
    };
    Err(CatalogError::UnknownEntry {
        dir: dir.display().to_string(),
        name: name.to_string(),
        available,
    })
}

fn is_yaml_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yaml") | Some("yml")
    )
}

/// [`PackResolver`] over a `--catalog <dir>` directory chained in front of
/// the [`builtin`] catalog.
///
/// Three cases, in order:
///
/// 1. A reference containing `/` or starting with `.` is a file path (spec
///    §2.4) and is read directly.
/// 2. A name found in `--catalog <dir>` resolves from there. The user wins:
///    a directory pack named `node_exporter_cpu` hides the builtin one.
/// 3. Otherwise the builtin catalog answers, which is what makes `pack:`
///    references work with no `--catalog` at all.
pub struct CatalogPackResolver<'a> {
    catalog_dir: Option<&'a Path>,
}

impl<'a> CatalogPackResolver<'a> {
    pub fn new(catalog_dir: Option<&'a Path>) -> Self {
        Self { catalog_dir }
    }

    /// Read a named pack out of the user directory, if it holds one.
    ///
    /// `Ok(None)` means "not in this directory, try the builtins";
    /// `Err` means the directory itself could not be enumerated, which is a
    /// real failure and must not fall through silently.
    fn read_from_user_dir(&self, reference: &str) -> Result<Option<String>, PackResolveError> {
        let Some(dir) = self.catalog_dir else {
            return Ok(None);
        };
        let entries = enumerate(dir).map_err(|e| {
            PackResolveError::new(
                format!("cannot enumerate catalog dir {}: {e}", dir.display()),
                PackResolveOrigin::Name,
            )
        })?;
        let Some(entry) = entries
            .iter()
            .find(|e| e.name == reference && e.kind == EntryKind::Composable)
        else {
            return Ok(None);
        };
        fs::read_to_string(&entry.source_path)
            .map(Some)
            .map_err(|e| {
                PackResolveError::new(
                    format!("cannot read pack file {}: {e}", entry.source_path.display()),
                    PackResolveOrigin::Name,
                )
            })
    }

    /// The "unknown pack" diagnostic, naming both halves of the chain so the
    /// user can see what each contributed.
    fn unknown_pack_error(&self, reference: &str) -> PackResolveError {
        let mut sources = Vec::new();
        if let Some(dir) = self.catalog_dir {
            let available = match enumerate(dir) {
                Ok(entries) => {
                    let composable: Vec<&str> = entries
                        .iter()
                        .filter(|e| e.kind == EntryKind::Composable)
                        .map(|e| e.name.as_str())
                        .collect();
                    if composable.is_empty() {
                        "<none>".to_string()
                    } else {
                        composable.join(", ")
                    }
                }
                Err(e) => format!("<unreadable: {e}>"),
            };
            sources.push(format!("catalog {}: {available}", dir.display()));
        }
        sources.push(format!("builtin: {}", builtin::names().join(", ")));
        // With no directory configured, the builtins are the whole search
        // path — so say how to extend it. Without this the message lists
        // three packs that are not the one you asked for and stops.
        let hint = if self.catalog_dir.is_none() {
            "; pass --catalog <dir> to resolve packs of your own"
        } else {
            ""
        };
        PackResolveError::new(
            format!(
                "unknown pack {reference:?}; composable entries: {}{hint}",
                sources.join("; ")
            ),
            PackResolveOrigin::Name,
        )
    }
}

impl<'a> PackResolver for CatalogPackResolver<'a> {
    fn resolve(&self, reference: &str) -> Result<MetricPackDef, PackResolveError> {
        let origin = classify_pack_reference(reference);
        let yaml = match origin {
            PackResolveOrigin::FilePath => fs::read_to_string(reference).map_err(|e| {
                PackResolveError::new(format!("cannot read pack file {reference:?}: {e}"), origin)
            })?,
            PackResolveOrigin::Name => match self.read_from_user_dir(reference)? {
                Some(yaml) => yaml,
                None => match builtin::find(reference) {
                    Some(pack) => pack.yaml.to_string(),
                    None => return Err(self.unknown_pack_error(reference)),
                },
            },
        };
        serde_yaml_ng::from_str::<MetricPackDef>(&yaml).map_err(|e| {
            PackResolveError::new(
                format!("cannot parse pack definition for {reference:?}: {e}"),
                origin,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, content: &str) {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).expect("create file");
        f.write_all(content.as_bytes()).expect("write file");
    }

    fn temp_catalog() -> TempDir {
        let dir = TempDir::new().expect("temp dir");
        write(
            dir.path(),
            "cpu-spike.yaml",
            r#"version: 2
kind: runnable
scenario_name: cpu-spike
description: CPU spike test
tags: [infrastructure, cpu]

defaults:
  rate: 1
  duration: 1s

scenarios:
  - id: a
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 1.0
"#,
        );
        write(
            dir.path(),
            "tiny-pack.yaml",
            r#"version: 2
kind: composable
scenario_name: tiny_pack
description: A small pack
tags: [network]

name: tiny_pack
category: network
metrics:
  - name: pack_metric_a
    generator:
      type: constant
      value: 1
"#,
        );
        write(dir.path(), "not-a-scenario.txt", "ignored");
        write(dir.path(), "missing-kind.yaml", "version: 2\n");
        dir
    }

    #[test]
    fn enumerate_returns_runnable_and_composable_sorted_by_name() {
        let tmp = temp_catalog();
        let entries = enumerate(tmp.path()).expect("must enumerate");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["cpu-spike", "tiny_pack"]);
    }

    #[test]
    fn enumerate_skips_files_without_kind() {
        let tmp = temp_catalog();
        let entries = enumerate(tmp.path()).expect("must enumerate");
        assert!(entries.iter().all(|e| e.name != "missing-kind"));
    }

    #[test]
    fn enumerate_preserves_tags() {
        let tmp = temp_catalog();
        let entries = enumerate(tmp.path()).expect("must enumerate");
        let cpu = entries.iter().find(|e| e.name == "cpu-spike").unwrap();
        assert_eq!(cpu.tags, vec!["infrastructure", "cpu"]);
    }

    #[test]
    fn enumerate_classifies_runnable_and_composable_kinds() {
        let tmp = temp_catalog();
        let entries = enumerate(tmp.path()).expect("must enumerate");
        let cpu = entries.iter().find(|e| e.name == "cpu-spike").unwrap();
        let pack = entries.iter().find(|e| e.name == "tiny_pack").unwrap();
        assert_eq!(cpu.kind, EntryKind::Runnable);
        assert_eq!(pack.kind, EntryKind::Composable);
    }

    #[test]
    fn resolve_returns_path_for_known_name() {
        let tmp = temp_catalog();
        let resolved = resolve(tmp.path(), "cpu-spike").expect("must resolve");
        assert_eq!(resolved.file_name().unwrap(), "cpu-spike.yaml");
    }

    #[test]
    fn resolve_returns_error_for_unknown_name() {
        let tmp = temp_catalog();
        let err = resolve(tmp.path(), "missing").expect_err("must error");
        let msg = format!("{err}");
        assert!(msg.contains("missing"), "got: {msg}");
        assert!(msg.contains("cpu-spike"), "must list candidates: {msg}");
    }

    #[cfg(unix)]
    #[test]
    fn enumerate_skips_an_unreadable_file_and_keeps_the_rest() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = temp_catalog();
        let locked = tmp.path().join("locked.yaml");
        write(tmp.path(), "locked.yaml", "version: 2\nkind: runnable\n");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000))
            .expect("must drop read permission");
        if fs::File::open(&locked).is_ok() {
            eprintln!("skipping: this process can open a 0o000 file (running as root?)");
            return;
        }

        let entries = enumerate(tmp.path()).expect("one unreadable file must not fail the catalog");

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["cpu-spike", "tiny_pack"]);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_finds_a_readable_entry_next_to_an_unreadable_file() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = temp_catalog();
        let locked = tmp.path().join("locked.yaml");
        write(tmp.path(), "locked.yaml", "version: 2\nkind: runnable\n");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000))
            .expect("must drop read permission");
        if fs::File::open(&locked).is_ok() {
            eprintln!("skipping: this process can open a 0o000 file (running as root?)");
            return;
        }

        let resolved = resolve(tmp.path(), "cpu-spike").expect("must resolve");

        assert_eq!(resolved.file_name().unwrap(), "cpu-spike.yaml");
    }

    #[test]
    fn enumerate_errors_on_nonexistent_dir() {
        let err = enumerate(Path::new("/nonexistent/sonda/catalog")).expect_err("must error");
        let msg = format!("{err}");
        assert!(msg.contains("does not exist"), "got: {msg}");
    }

    // ---- skip reporting ---------------------------------------------------

    /// The silent skip, made visible. `missing-kind.yaml` is a real YAML
    /// file the walk drops; before this it left no trace anywhere.
    #[test]
    fn enumerate_with_skips_names_the_file_that_has_no_kind_header() {
        let tmp = temp_catalog();
        let listing = enumerate_with_skips(tmp.path()).expect("must enumerate");

        let skipped: Vec<&SkippedFile> = listing
            .skipped
            .iter()
            .filter(|s| s.reason == SkipReason::MissingKind)
            .collect();
        assert_eq!(
            skipped.len(),
            1,
            "expected exactly missing-kind.yaml, got {:?}",
            listing.skipped
        );
        assert_eq!(
            skipped[0].path.file_name().unwrap(),
            "missing-kind.yaml",
            "got: {}",
            skipped[0].path.display()
        );
        assert!(skipped[0].reason.describe().contains("kind"));
    }

    /// A `.txt` file is not a catalog candidate at all, so it must not show
    /// up as a skip — a note listing every unrelated file in the directory
    /// would be noise, not honesty.
    #[test]
    fn a_non_yaml_file_is_not_reported_as_skipped() {
        let tmp = temp_catalog();
        let listing = enumerate_with_skips(tmp.path()).expect("must enumerate");
        assert!(
            !listing
                .skipped
                .iter()
                .any(|s| s.path.extension().and_then(|e| e.to_str()) == Some("txt")),
            "got: {:?}",
            listing.skipped
        );
    }

    #[test]
    fn an_unrecognized_kind_is_reported_with_the_value_the_file_declared() {
        let tmp = temp_catalog();
        write(tmp.path(), "odd.yaml", "version: 2\nkind: sideways\n");
        let listing = enumerate_with_skips(tmp.path()).expect("must enumerate");

        let odd = listing
            .skipped
            .iter()
            .find(|s| s.path.file_name().unwrap() == "odd.yaml")
            .expect("odd.yaml must be reported");
        assert_eq!(odd.reason, SkipReason::UnknownKind("sideways".to_string()));
        assert!(odd.reason.describe().contains("sideways"));
    }

    // ---- merged listing ---------------------------------------------------

    #[test]
    fn merged_with_no_user_dir_is_exactly_the_builtin_set() {
        let listing = merged(None).expect("builtins alone must list");
        assert_eq!(listing.entries.len(), builtin::PACK_COUNT);
        assert!(listing
            .entries
            .iter()
            .all(|e| e.origin == EntryOrigin::Builtin));
        assert!(
            listing.entries.iter().all(|e| !e.shadows_builtin),
            "nothing can shadow a builtin when there is no user dir"
        );
        assert!(listing.skipped.is_empty());
    }

    #[test]
    fn merged_keeps_both_sources_when_no_name_collides() {
        let tmp = temp_catalog();
        let listing = merged(Some(tmp.path())).expect("must merge");
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();

        assert!(names.contains(&"cpu-spike"), "user entry: {names:?}");
        assert!(names.contains(&"node_exporter_cpu"), "builtin: {names:?}");
        assert_eq!(names.len(), 2 + builtin::PACK_COUNT);
        assert!(listing.entries.iter().all(|e| !e.shadows_builtin));
    }

    /// A user pack named after a builtin replaces it: one row, flagged.
    /// The "absent without" direction is
    /// `merged_with_no_user_dir_is_exactly_the_builtin_set`.
    #[test]
    fn merged_marks_a_user_pack_that_shadows_a_builtin_and_drops_the_builtin_row() {
        let tmp = temp_catalog();
        write(
            tmp.path(),
            "my-cpu.yaml",
            r#"version: 2
kind: composable
name: node_exporter_cpu
description: My own CPU pack
category: infrastructure

metrics:
  - name: node_cpu_seconds_total
    generator:
      type: constant
      value: 99
"#,
        );

        let listing = merged(Some(tmp.path())).expect("must merge");
        let rows: Vec<&CatalogEntry> = listing
            .entries
            .iter()
            .filter(|e| e.name == "node_exporter_cpu")
            .collect();

        assert_eq!(rows.len(), 1, "the shadowed builtin must not list twice");
        assert_eq!(rows[0].origin, EntryOrigin::UserDir);
        assert!(rows[0].shadows_builtin, "the winner must carry the marker");
        assert_eq!(rows[0].description, "My own CPU pack");
    }

    // ---- resolver chain ---------------------------------------------------

    #[test]
    fn resolver_falls_back_to_the_builtin_catalog_with_no_catalog_dir() {
        let resolver = CatalogPackResolver::new(None);
        let pack = resolver
            .resolve("telegraf_snmp_interface")
            .expect("a builtin must resolve with zero setup");
        assert_eq!(pack.name, "telegraf_snmp_interface");
    }

    /// User wins. The distinguishing value is the metric list: the builtin
    /// `node_exporter_cpu` ships eight `node_cpu_seconds_total` specs, this
    /// one ships a single `only_in_the_user_copy`.
    #[test]
    fn resolver_prefers_a_user_dir_pack_over_the_builtin_of_the_same_name() {
        let tmp = temp_catalog();
        write(
            tmp.path(),
            "my-cpu.yaml",
            r#"version: 2
kind: composable
name: node_exporter_cpu
description: My own CPU pack
category: infrastructure

metrics:
  - name: only_in_the_user_copy
    generator:
      type: constant
      value: 99
"#,
        );

        let shadowed = CatalogPackResolver::new(Some(tmp.path()))
            .resolve("node_exporter_cpu")
            .expect("the user copy must resolve");
        let builtin_copy = CatalogPackResolver::new(None)
            .resolve("node_exporter_cpu")
            .expect("the builtin must resolve");

        let user_metrics: Vec<&str> = shadowed.metrics.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(user_metrics, vec!["only_in_the_user_copy"]);
        assert!(
            builtin_copy
                .metrics
                .iter()
                .all(|m| m.name == "node_cpu_seconds_total"),
            "the builtin must be untouched by the shadowing"
        );
    }

    #[test]
    fn resolver_still_finds_a_builtin_when_the_user_dir_holds_other_packs() {
        let tmp = temp_catalog();
        let resolver = CatalogPackResolver::new(Some(tmp.path()));
        assert_eq!(
            resolver
                .resolve("tiny_pack")
                .expect("user pack must resolve")
                .name,
            "tiny_pack"
        );
        assert_eq!(
            resolver
                .resolve("node_exporter_memory")
                .expect("builtin must resolve behind the user dir")
                .name,
            "node_exporter_memory"
        );
    }

    /// With no directory configured the builtins are the whole search path,
    /// so the message has to say how to widen it — otherwise it lists three
    /// packs that are not the one you asked for and stops.
    #[test]
    fn resolver_unknown_name_with_no_catalog_dir_says_how_to_add_one() {
        let err = CatalogPackResolver::new(None)
            .resolve("tiny_pack")
            .expect_err("must error");
        let msg = format!("{err}");
        assert!(msg.contains("tiny_pack"), "must name the pack: {msg}");
        assert!(
            msg.contains("--catalog"),
            "must say how to supply it: {msg}"
        );
    }

    /// The converse: with a directory configured, the message names it and
    /// what it holds, and does not tell the user to pass a flag they passed.
    #[test]
    fn resolver_unknown_name_with_a_catalog_dir_does_not_repeat_the_flag() {
        let tmp = temp_catalog();
        let err = CatalogPackResolver::new(Some(tmp.path()))
            .resolve("no_such_pack")
            .expect_err("must error");
        let msg = format!("{err}");
        assert!(
            !msg.contains("pass --catalog"),
            "the flag was already given: {msg}"
        );
    }

    #[test]
    fn resolver_unknown_name_names_both_halves_of_the_chain() {
        let tmp = temp_catalog();
        let err = CatalogPackResolver::new(Some(tmp.path()))
            .resolve("no_such_pack")
            .expect_err("must error");
        let msg = format!("{err}");
        assert!(msg.contains("no_such_pack"), "got: {msg}");
        assert!(msg.contains("tiny_pack"), "must name the catalog: {msg}");
        assert!(
            msg.contains("node_exporter_cpu"),
            "must name the builtins: {msg}"
        );
    }

    /// An unreadable catalog directory is a real failure. It must not fall
    /// through to the builtins and answer as if nothing were wrong.
    #[test]
    fn resolver_reports_an_unusable_catalog_dir_instead_of_silently_using_builtins() {
        let resolver = CatalogPackResolver::new(Some(Path::new("/nonexistent/sonda/catalog")));
        let err = resolver
            .resolve("node_exporter_cpu")
            .expect_err("a broken --catalog must not be masked by the builtin fallback");
        assert!(
            format!("{err}").contains("cannot enumerate catalog dir"),
            "got: {err}"
        );
    }
}
