# Parsers

`sonda parsers` is the entry point for converting external byte streams into the canonical Sonda log CSV plus a runnable v2 scenario YAML. Each subcommand under `parsers` is a parser **family**: `rawlog` for line-oriented log files today, more (e.g. `metriccsv`) on the roadmap.

This page is the reference surface for every `parsers` subcommand. For the end-to-end workflow with worked examples, see the [Raw Log Parser guide](../guides/rawlog-parser.md).

---

## The layered model

```
  ┌─────────────────────────────┐       ┌─────────────────────────────┐
  │ sonda-parsers               │       │ sonda-fetcher (future)      │
  │ in-tree Rust subcrate       │       │ separate Python project     │
  │ bytes on disk -> canonical  │       │ APIs -> bytes on disk       │
  │   CSV + scenario YAML       │       │   (Grafana, Loki, …)        │
  │                             │       │                             │
  │ rawlog (plain, nginx)       │       │                             │
  └─────────────────────────────┘       └─────────────────────────────┘
                 │                                  │
                 └──────────────┬───────────────────┘
                                ▼  canonical log CSV
                       sonda-core (log_csv_replay)
```

`sonda-parsers` ships in the Sonda binary. It handles **log files you already have on disk**. The downstream consumer is always [`log_csv_replay`](../guides/log-csv-replay.md), the canonical log-replay generator.

`sonda-fetcher` is a separate companion project (Python, in development) that will pull bytes **live from observability backends** -- Grafana, Loki, Prometheus -- and write the same canonical CSV that the parsers produce. Until it lands, export the window yourself and feed the file to `sonda parsers`.

---

## `sonda parsers rawlog`

Convert a line-oriented log file into the canonical CSV + scenario YAML.

```bash
sonda parsers rawlog <FILE> --format <FORMAT> -o <OUTPUT> [OPTIONS]
```

### Arguments and flags

| Argument / Flag | Type | Required | Default | Description |
|-----------------|------|----------|---------|-------------|
| `<FILE>` | path | yes | -- | Path to the input log file. Read fully into memory; the parser is synchronous. |
| `--format <FORMAT>` | string | yes | -- | Log line format to parse. One of `plain`, `nginx`. Case-sensitive. |
| `-o, --output <OUTPUT>` | path | yes | -- | Path to write the emitted scenario YAML. The companion CSV lands in the same directory, named after the input log's file stem. |
| `--delta-seconds <SECONDS>` | float | no | `1.0` | Step in seconds between synthesized timestamps for rows that have no parseable timestamp (always for `plain`, never for valid `nginx` lines). |
| `--scenario-name <NAME>` | string | no | `<file_stem>_replay` | Override the `name:` field written into the emitted YAML's scenario entry. |

### Output

Two files are written on success:

- The scenario YAML at `--output`.
- The canonical CSV at `<output_dir>/<input_stem>.csv`. For example, an input of `examples/sample-nginx.log` with `-o /tmp/out.yaml` produces `/tmp/sample-nginx.csv`.

The CSV path stored **inside** the YAML is relative to the YAML's parent directory, so the pair is portable -- commit them together, copy them to a server, and `sonda run` resolves the CSV correctly.

A status block prints to stderr on success:

```text
parsed 20 rows from "examples/sample-nginx.log" (format: nginx)
wrote csv:  examples/sample-nginx.csv
wrote yaml: examples/rawlog-nginx-replay.yaml
validate:   sonda --dry-run run --scenario examples/rawlog-nginx-replay.yaml
```

### Emitted YAML shape

```yaml
version: 2
defaults:
  rate: 1                    # informational; log_csv_replay derives from CSV Δt
  duration: <N>s             # source CSV span rounded up; or row_count * delta_seconds when synthesized
  encoder:
    type: json_lines
  sink:
    type: stdout
scenarios:
- signal_type: logs
  name: <scenario_name>      # from --scenario-name, or <file_stem>_replay
  log_generator:
    type: csv_replay
    file: <stem>.csv         # relative to this YAML's parent dir
    default_severity: info
    repeat: false
```

The defaults are deliberately minimal: `stdout` sink, `json_lines` encoder, `repeat: false`. Edit the YAML to swap in a real sink (`loki`, `http_push`, `file`, …) or to add `timescale:` under `log_generator:` to speed up or slow down the replay.

### Canonical CSV shape

The header is always:

```csv
timestamp,severity,message[,...field_columns]
```

- `timestamp` -- epoch seconds (real for `nginx`, synthesized for `plain`).
- `severity` -- lowercase string (`info`, `warn`, `error`, `debug`, `trace`, `fatal`) or empty (falls back to `default_severity` at replay).
- `message` -- free-form text.
- Field columns appear after `message` in alphabetical order, deterministic across runs.

RFC 4180 quoting is applied to any cell containing a comma, double-quote, newline, or carriage return.

---

## Format: `plain`

Every non-blank input line becomes one row. The whole line is the `message`. No timestamp parsing, no severity discovery, no field discovery.

| Output column | Behavior |
|---------------|----------|
| `timestamp` | Synthesized: starts at `1700000000.0`, increments by `--delta-seconds` per row. |
| `severity` | Empty cell. Replay falls back to `default_severity: info`. |
| `message` | The line, trimmed of surrounding whitespace. |
| field columns | None. |

Blank lines and whitespace-only lines are skipped. The synthesized timestamps guarantee monotonic increase so `log_csv_replay`'s non-monotonic detector never fires.

Use `--delta-seconds` to control the replay cadence: `0.1` for ten events per second, `60` for one per minute.

---

## Format: `nginx`

Parses NGINX combined log format:

```
$remote_addr - $remote_user [$time_local] "$request" $status $body_bytes_sent "$http_referer" "$http_user_agent"
```

| Output column | Source |
|---------------|--------|
| `timestamp` | Bracketed `[DD/Mon/YYYY:HH:MM:SS +ZZZZ]` field, converted to epoch seconds with the timezone offset applied. |
| `severity` | HTTP status code -> severity. See table below. |
| `message` | `<request line> <status>` (e.g. `GET /api/v1/users HTTP/1.1 200`). |
| `method` | First token of the request line. |
| `path` | Second token of the request line. |
| `remote_addr` | First whitespace-separated token of the line. |
| `status` | Numeric status code as a string. |
| `user_agent` | The second quoted string after the status/bytes pair. |

### Status-code severity mapping

| Status range | Severity |
|--------------|----------|
| `100-199`, `200-299`, `300-399` | `info` |
| `400-499` | `warn` |
| `500-599` | `error` |
| any other value | `info` (with a warn-level log noting "unrecognized status code") |

Timezone offsets are normalized to UTC: a `+0530` timestamp is rolled back 5h30m before being stored as epoch seconds, so all CSVs share a single reference frame.

### Lines that fail to parse

Lines that do not match the combined format -- missing brackets, missing quoted request, non-numeric status -- are **skipped silently**. The parser does not exit on a per-line failure; it produces a CSV containing only the rows that parsed. If every line fails, the parser exits with `input file "..." contains no parseable rows`.

---

## Edge cases and failure modes

| Situation | Behavior |
|-----------|----------|
| Empty file, or only blank lines | Exit non-zero: `input file "<path>" contains no parseable rows`. |
| Unknown `--format` value | Exit non-zero: `unknown format "<name>": must be one of ["plain", "nginx"]`. |
| `<FILE>` does not exist | Exit non-zero: `input file "<path>" could not be read: No such file or directory`. |
| `-o` parent directory does not exist | Exit non-zero on write. Create the directory first. |
| `nginx` line missing the bracketed timestamp | Row skipped. Other rows still produce output. |
| `nginx` status code outside 100-599 | Row kept; severity = `info`; one warn-level summary line per parse run. |
| CSV field contains `,` or `"` | RFC 4180 quoting applied. |
| Mixed `nginx` and non-`nginx` lines in one file | Only the matching lines make it to the CSV. |

---

## Round-trip workflow

Every invocation pairs with two follow-up commands:

```bash
# 1. parse
sonda parsers rawlog access.log --format nginx -o access-replay.yaml

# 2. dry-run -- confirms YAML validates and shows resolved config
sonda --dry-run run --scenario access-replay.yaml

# 3. run -- replays through the configured sink
sonda run --scenario access-replay.yaml
```

The dry-run step is the one quick check that catches a downstream mistake (typo in the emitted YAML, missing CSV, unreadable file path) before you commit the pair to a repo.

---

## Related

- [Raw Log Parser guide](../guides/rawlog-parser.md) -- end-to-end walkthroughs with real input and output.
- [Log CSV Replay](../guides/log-csv-replay.md) -- the downstream generator that consumes the emitted CSV.
- [CLI reference: `sonda parsers`](cli-reference.md#sonda-parsers) -- the canonical flag table.
