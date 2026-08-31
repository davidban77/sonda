//! Integration tests for `sonda list` and `sonda show`.

mod common;

use std::io::Write;
use std::process::Command;

use common::sonda_bin;
use sonda_core::catalog::builtin::PACK_COUNT;
use tempfile::TempDir;

/// A user pack that takes the name of a builtin. Its single metric is the
/// distinguishing value: the builtin `node_exporter_cpu` ships eight
/// `node_cpu_seconds_total` specs and nothing called this.
const SHADOWING_PACK_YAML: &str = "version: 2
kind: composable
name: node_exporter_cpu
description: My own CPU pack
category: infrastructure

metrics:
  - name: only_in_the_user_copy
    generator:
      type: constant
      value: 99.0
";

const RUNNABLE_YAML: &str = "version: 2
kind: runnable
scenario_name: cpu-spike
description: A CPU spike scenario
tags: [infrastructure, cpu]

defaults:
  rate: 1
  duration: 1s
  encoder:
    type: prometheus_text
  sink:
    type: stdout

scenarios:
  - id: m
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 1.0
";

const PACK_YAML: &str = "version: 2
kind: composable
scenario_name: tiny-pack
description: A small pack
tags: [network]

name: tiny_pack
category: network
metrics:
  - name: pack_metric_a
    generator:
      type: constant
      value: 1.0
";

fn write_catalog() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let runnable_path = dir.path().join("cpu-spike.yaml");
    std::fs::File::create(&runnable_path)
        .expect("create")
        .write_all(RUNNABLE_YAML.as_bytes())
        .expect("write");
    let pack_path = dir.path().join("tiny-pack.yaml");
    std::fs::File::create(&pack_path)
        .expect("create")
        .write_all(PACK_YAML.as_bytes())
        .expect("write");
    dir
}

#[test]
fn list_prints_all_entries() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["list"])
        .output()
        .expect("spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("cpu-spike"),
        "must list cpu-spike, got: {stdout}"
    );
    assert!(
        stdout.contains("tiny-pack"),
        "must list tiny-pack, got: {stdout}"
    );
    assert!(
        stdout.contains("KIND"),
        "must include header, got: {stdout}"
    );
    assert!(stdout.contains("runnable"));
    assert!(stdout.contains("composable"));
    assert!(
        stdout.contains("infrastructure"),
        "tags must be present, got: {stdout}"
    );
}

#[test]
fn list_filters_by_kind_runnable() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["list", "--kind", "runnable"])
        .output()
        .expect("spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cpu-spike"), "got: {stdout}");
    assert!(
        !stdout.contains("tiny-pack"),
        "composable must be filtered out, got: {stdout}"
    );
}

#[test]
fn list_filters_by_kind_composable() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["list", "--kind", "composable"])
        .output()
        .expect("spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("tiny-pack"));
    assert!(!stdout.contains("cpu-spike"));
}

#[test]
fn list_filters_by_tag() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["list", "--tag", "network"])
        .output()
        .expect("spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("tiny-pack"), "got: {stdout}");
    assert!(!stdout.contains("cpu-spike"));
}

#[test]
fn list_json_output_is_machine_readable() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["list", "--json"])
        .output()
        .expect("spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("must be valid JSON");
    let arr = parsed.as_array().expect("must be array");
    assert_eq!(
        arr.len(),
        2 + PACK_COUNT,
        "the two catalog entries plus every builtin, got: {stdout}"
    );
    for entry in arr {
        let obj = entry.as_object().expect("object");
        assert!(obj.contains_key("name"));
        assert!(obj.contains_key("kind"));
        assert!(obj.contains_key("description"));
        assert!(obj.contains_key("tags"));
        assert!(obj.contains_key("category"));
        assert!(obj.contains_key("origin"));
        assert!(obj.contains_key("shadows_builtin"));
    }
}

// ---- the builtin catalog ------------------------------------------------------

/// The zero-setup path: no `--catalog`, no files, still a catalog.
#[test]
fn list_with_no_catalog_prints_the_builtin_packs() {
    let output = Command::new(sonda_bin())
        .args(["list"])
        .output()
        .expect("spawn sonda");
    assert!(
        output.status.success(),
        "`sonda list` must work with no arguments; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for name in [
        "node_exporter_cpu",
        "node_exporter_memory",
        "telegraf_snmp_interface",
    ] {
        assert!(stdout.contains(name), "must list {name}, got: {stdout}");
    }
}

#[test]
fn list_json_with_no_catalog_is_exactly_the_builtin_set() {
    let output = Command::new(sonda_bin())
        .args(["list", "--json"])
        .output()
        .expect("spawn sonda");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("must be valid JSON");
    let arr = parsed.as_array().expect("must be array");
    assert_eq!(arr.len(), PACK_COUNT, "got: {stdout}");
    for entry in arr {
        assert_eq!(entry["origin"], "builtin", "got: {entry}");
        assert_eq!(
            entry["shadows_builtin"], false,
            "nothing can shadow a builtin with no catalog dir: {entry}"
        );
    }
}

/// Grouped by `category:`, which is what keeps the flat directory readable.
#[test]
fn list_groups_entries_under_their_category() {
    let output = Command::new(sonda_bin())
        .args(["list"])
        .output()
        .expect("spawn sonda");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[infrastructure]"), "got: {stdout}");
    assert!(stdout.contains("[network]"), "got: {stdout}");

    let infra = stdout
        .find("[infrastructure]")
        .expect("infrastructure group");
    let network = stdout.find("[network]").expect("network group");
    let cpu = stdout.find("node_exporter_cpu").expect("cpu pack");
    let snmp = stdout.find("telegraf_snmp_interface").expect("snmp pack");
    assert!(
        infra < cpu && cpu < network,
        "cpu belongs to infrastructure: {stdout}"
    );
    assert!(network < snmp, "snmp belongs to network: {stdout}");
}

/// A pack in `--catalog <dir>` named after a builtin wins, and says so.
/// The other direction — no marker without the user dir — is
/// `list_json_with_no_catalog_is_exactly_the_builtin_set`.
#[test]
fn list_marks_a_user_pack_that_shadows_a_builtin() {
    let cat = write_catalog();
    std::fs::write(cat.path().join("mine.yaml"), SHADOWING_PACK_YAML).expect("write shadow pack");

    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["list"])
        .output()
        .expect("spawn sonda");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("node_exporter_cpu (shadows builtin)"),
        "the winner must be marked, got: {stdout}"
    );
    assert!(
        stdout.contains("My own CPU pack"),
        "the user's description must be the one shown, got: {stdout}"
    );
    assert_eq!(
        stdout.matches("node_exporter_cpu").count(),
        1,
        "the hidden builtin must not also list, got: {stdout}"
    );
}

/// The silent skip, made visible. `enumerate` drops a YAML file with no
/// `kind:` header; a skip the user cannot see reads as coverage.
#[test]
fn list_names_a_yaml_file_it_skipped_for_having_no_kind_header() {
    let cat = write_catalog();
    std::fs::write(cat.path().join("notes.yaml"), "version: 2\njust: data\n")
        .expect("write non-entry YAML");

    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["list"])
        .output()
        .expect("spawn sonda");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("notes.yaml"),
        "the skipped file must be named: {stderr}"
    );
    assert!(
        stderr.contains("kind"),
        "the note must say why it was skipped: {stderr}"
    );
}

#[test]
fn show_prints_a_builtin_pack_with_no_catalog() {
    let output = Command::new(sonda_bin())
        .args(["show", "@node_exporter_cpu"])
        .output()
        .expect("spawn sonda");
    assert!(
        output.status.success(),
        "`sonda show` must reach the builtins; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("kind: composable"), "got: {stdout}");
    assert!(stdout.contains("node_cpu_seconds_total"), "got: {stdout}");
    assert!(
        stdout.contains("name: node_exporter_cpu"),
        "the embedded YAML verbatim, got: {stdout}"
    );
}

/// `show` reads the embedded bytes, never the `<builtin>/…` marker that
/// stands in for a source path.
#[test]
fn show_of_a_builtin_matches_the_file_in_the_packs_directory() {
    let output = Command::new(sonda_bin())
        .args(["show", "@telegraf_snmp_interface"])
        .output()
        .expect("spawn sonda");
    assert!(output.status.success());
    let on_disk = std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("packs/telegraf-snmp-interface.yaml"),
    )
    .expect("the source pack must exist at packs/");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        on_disk,
        "the embedded copy must be the file in packs/, byte for byte"
    );
}

#[test]
fn show_unknown_name_lists_what_is_available() {
    let output = Command::new(sonda_bin())
        .args(["show", "@no_such_entry"])
        .output()
        .expect("spawn sonda");
    assert!(!output.status.success(), "unknown entry must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no_such_entry"), "got: {stderr}");
    assert!(
        stderr.contains("node_exporter_cpu"),
        "must name the builtins it does have: {stderr}"
    );
}

#[test]
fn show_runnable_prints_raw_yaml_that_round_trips() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["show", "@cpu-spike"])
        .output()
        .expect("spawn sonda");
    assert!(
        output.status.success(),
        "show runnable must succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cpu_usage"), "got: {stdout}");
    assert!(
        stdout.contains("kind: runnable"),
        "expected kind: runnable in output, got: {stdout}"
    );

    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    std::fs::write(tmp.path(), stdout.as_bytes()).expect("write tempfile");
    let dry = Command::new(sonda_bin())
        .arg("--dry-run")
        .args(["run"])
        .arg(tmp.path())
        .output()
        .expect("spawn sonda --dry-run");
    assert!(
        dry.status.success(),
        "show output must round-trip through `sonda --dry-run run`; stderr:\n{}",
        String::from_utf8_lossy(&dry.stderr)
    );
}

#[test]
fn show_composable_prints_raw_yaml() {
    let cat = write_catalog();
    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["show", "@tiny-pack"])
        .output()
        .expect("spawn sonda");
    assert!(
        output.status.success(),
        "show composable must succeed; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("kind: composable"),
        "raw YAML expected, got: {stdout}"
    );
    assert!(stdout.contains("pack_metric_a"));
}

#[cfg(unix)]
#[test]
fn list_skips_an_unreadable_file_and_prints_the_rest() {
    use std::os::unix::fs::PermissionsExt;

    let cat = write_catalog();
    let locked = cat.path().join("locked.yaml");
    std::fs::write(&locked, "version: 2\nkind: runnable\n").expect("write locked entry");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
        .expect("drop read permission");
    if std::fs::File::open(&locked).is_ok() {
        eprintln!("skipping: this process can open a 0o000 file (running as root?)");
        return;
    }

    let output = Command::new(sonda_bin())
        .args(["--catalog"])
        .arg(cat.path())
        .args(["list"])
        .output()
        .expect("spawn sonda");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stdout.contains("cpu-spike") && stdout.contains("tiny-pack"),
        "one unreadable file must not cost the catalog, got: {stdout}"
    );
    assert!(
        stderr.contains("locked.yaml"),
        "the skipped file must be named: {stderr}"
    );
}
