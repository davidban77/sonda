# Catalogs

A **catalog** is a directory of v2 YAML files that Sonda discovers via `--catalog <dir>`. Each file declares a `kind:` — `runnable` for scenarios you can run, `composable` for [metric packs](metric-packs.md) other scenarios reference. Sonda doesn't ship a built-in catalog: yours lives in your own repo, versioned next to your alert rules, dashboards, and CI workflows. Scenarios become first-class artifacts of the system they model instead of being pinned to a Sonda release.

## The minimum

```text
my-catalog/
├── cpu-spike.yaml          # kind: runnable
├── memory-leak.yaml        # kind: runnable
└── prom-text-stdout.yaml   # kind: composable  (a pack)
```

```bash
sonda --catalog ./my-catalog list
sonda --catalog ./my-catalog show @cpu-spike
sonda --catalog ./my-catalog run @cpu-spike
```

Files without a recognized `kind:` header are silently skipped. Files with an unparseable YAML
body print a warning to stderr and are skipped — the listing continues.

Two files with the same logical name (`name:` field or filename) are a **hard error** —
discovery fails with the conflicting paths. Rename one to disambiguate.

## Browse the catalog

`sonda list` prints a tab-separated table of every entry in the catalog:

```bash
sonda --catalog ~/sonda-catalog list
```

```text title="Output"
KIND        NAME              TAGS                  DESCRIPTION
runnable    cpu-spike         cpu,infrastructure    CPU spike to 95% for 30 seconds
runnable    memory-leak       memory,leak           Slow memory leak from baseline to ceiling
composable  prom-text-stdout  defaults              Shared prometheus_text + stdout defaults
```

Filter by entry kind or tag:

```bash
sonda --catalog ~/sonda-catalog list --kind runnable
sonda --catalog ~/sonda-catalog list --tag cpu
```

For machine-readable output, add `--json` to get a stable array on stdout. Each element has
`name`, `kind`, `description`, `tags`, and the resolved `source` path. Use it as the contract
when scripting catalog discovery.

## Run a scenario

`sonda run @name --catalog <dir>` resolves the name in the catalog and runs the entry:

```bash
sonda --catalog ~/sonda-catalog run @cpu-spike --rate 5 --duration 10s
```

```text title="Output"
▶ node_cpu_usage_percent  signal_type: metrics | rate: 5/s | encoder: prometheus_text | sink: stdout | duration: 10s
node_cpu_usage_percent{cpu="0",instance="web-01",job="node_exporter"} 95 1775589686141
node_cpu_usage_percent{cpu="0",instance="web-01",job="node_exporter"} 95 1775589686641
...
■ node_cpu_usage_percent  completed in 10.0s | events: 50 | bytes: 4350 B | errors: 0
```

`sonda run` also accepts a direct filesystem path (no `@`, no `--catalog`) when you want to
run a one-off file:

```bash
sonda run examples/basic-metrics.yaml
```

CLI overrides (`--duration`, `--rate`, `--sink`, `--endpoint`, `--encoder`, `--label`) win
over the values inside the file, so you can pin a backend or speed up a long-running scenario
without editing the YAML.

!!! tip "Validate without emitting"
    Add `--dry-run` to compile the scenario and print the resolved config — no events are
    written:

    ```bash
    sonda --catalog ~/sonda-catalog --dry-run run @cpu-spike
    ```

## Inspect the YAML

`sonda show @name --catalog <dir>` prints the file contents byte-for-byte:

```bash
sonda --catalog ~/sonda-catalog show @cpu-spike
```

```yaml title="Output"
# CPU spike: periodic CPU usage spikes above threshold.
version: 2
kind: runnable

name: cpu-spike
tags: [cpu, infrastructure]
description: "Periodic CPU usage spikes above threshold"

scenarios:
  - signal_type: metrics
    name: node_cpu_usage_percent
    rate: 1
    duration: 60s
    generator:
      type: spike_event
      baseline: 35.0
      spike_height: 60.0
      spike_duration: "10s"
      spike_interval: "30s"
    labels:
      instance: web-01
      job: node_exporter
      cpu: "0"
    encoder:
      type: prometheus_text
    sink:
      type: stdout
```

Pipe the output to a file when you want to fork an entry and customize it:

```bash title="my-cpu-spike.yaml"
sonda --catalog ~/sonda-catalog show @cpu-spike > my-cpu-spike.yaml
# edit my-cpu-spike.yaml — change labels, generator params, etc.
sonda run my-cpu-spike.yaml
```

## Author your own entries

A catalog entry is a v2 scenario YAML with a top-level `kind:` field. For runnable entries:

```yaml title="~/sonda-catalog/my-scenario.yaml"
version: 2
kind: runnable

name: my-scenario
tags: [application, custom]
description: "My custom scenario pattern"

scenarios:
  - id: my_metric
    signal_type: metrics
    name: my_metric
    rate: 1
    duration: 30s
    generator:
      type: sine
      amplitude: 50.0
      period_secs: 60
      offset: 50.0
    encoder:
      type: prometheus_text
    sink:
      type: stdout
```

| Field | Required | Description |
|-------|----------|-------------|
| `version` | yes | Must be `2`. |
| `kind` | yes | `runnable` for runnable scenarios; `composable` for packs (see [Metric Packs](metric-packs.md)). |
| `name` | no | Catalog identifier. Defaults to the filename (without `.yaml`) if omitted. Used with `@name`. |
| `tags` | no | Optional list of strings. `sonda list --tag <t>` filters on this. |
| `description` | no | One-line summary shown in the `sonda list` table and JSON output. |

The compiler ignores `tags:` and `description:` — they only feed the catalog views. Strict
unknown-field validation stays in force, so typos like `desc:` or `tag:` (singular) are
rejected at parse time.

After dropping the file in your catalog directory, `sonda list` picks it up on the next run:

```bash
sonda --catalog ~/sonda-catalog list --tag application
```

## What next

- [**Metric Packs**](metric-packs.md) -- pre-built metric bundles for Telegraf SNMP and
  node_exporter, expressed as `kind: composable` catalog entries.
- [**Alert Testing**](alert-testing.md) -- end-to-end walkthrough using shaped signals to
  validate alert rules.
- [**CLI Reference**](../configuration/cli-reference.md) -- full flag reference for `sonda run`,
  `sonda list`, `sonda show`, and `sonda new`.
- [**Scenario Fields**](../configuration/scenario-fields.md) -- YAML reference for writing your
  own scenarios from scratch.
- [**v2 Scenario Files**](../configuration/v2-scenarios.md) -- the canonical file format with
  defaults, `after:`, and inline packs.
