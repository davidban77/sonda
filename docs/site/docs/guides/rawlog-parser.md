# Raw Log Parser

You have a file of real log lines on disk -- an NGINX access log pulled off a server, an application's stdout captured during an incident, the trailing 10 MB of a Kubernetes pod log. You want to feed those exact lines back through your pipeline, with the right severities and the original timing, without writing a custom CSV converter first. `sonda parsers rawlog` is the tool: it reads the file, picks out the structured columns the format implies, and emits a canonical Sonda CSV plus a runnable v2 scenario YAML that points at it.

The emitted YAML uses [`log_csv_replay`](log-csv-replay.md), so you get its derived replay rate, severity fallback, and `timescale` controls for free. The parser is the one-step bridge from "log file on disk" to "log stream replayed against my pipeline".

!!! info "Where parsers fit"
    `sonda-parsers` converts log files **you already have on disk** into the canonical CSV. For pulling data **live from a backend** -- Grafana, Loki, Prometheus -- a separate companion tool (`sonda-fetcher`) is in development. Today, export the window yourself (e.g. with `logcli` or `curl`) and feed the file to `sonda parsers rawlog`.

---

## The workflow

Every invocation follows the same three steps. Pick the format, point at the file, then validate and run:

```bash
# 1. parse  -- writes <stem>.csv + your YAML
sonda parsers rawlog examples/sample-nginx.log --format nginx -o examples/rawlog-nginx-replay.yaml

# 2. dry-run -- confirms the YAML validates and shows the resolved config
sonda --dry-run run --scenario examples/rawlog-nginx-replay.yaml

# 3. run -- replays against your sink (stdout by default; swap to loki/file/http for real use)
sonda run --scenario examples/rawlog-nginx-replay.yaml
```

The exact "validate" command is printed on stderr by the parse step, so you can copy-paste it.

```text title="stderr after step 1"
parsed 20 rows from "examples/sample-nginx.log" (format: nginx)
wrote csv:  examples/sample-nginx.csv
wrote yaml: examples/rawlog-nginx-replay.yaml
validate:   sonda --dry-run run --scenario examples/rawlog-nginx-replay.yaml
```

Two files appear next to each other: `examples/sample-nginx.csv` (named after the input log's file stem) and the YAML you asked for. The CSV path **inside** the YAML is relative to the YAML's parent directory, so you can commit the pair to a repo or copy them to another machine and they still resolve.

---

## Format: `nginx`

NGINX's combined log format is the canonical access log shape. `--format nginx` parses the bracketed timestamp into epoch seconds and maps the HTTP status code to a severity, so a 4xx becomes a `warn` and a 5xx becomes an `error` without you doing anything.

```text title="examples/sample-nginx.log (first three lines)"
192.168.1.10 - - [15/May/2026:08:00:00 +0000] "GET /api/v1/users HTTP/1.1" 200 1234 "-" "Mozilla/5.0 (X11; Linux x86_64)"
192.168.1.12 - - [15/May/2026:08:00:04 +0000] "POST /api/v1/login HTTP/1.1" 401 64 "https://example.com/" "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)"
192.168.1.17 - - [15/May/2026:08:00:14 +0000] "POST /api/v1/orders HTTP/1.1" 500 256 "-" "kit/2.0"
```

```bash
sonda parsers rawlog examples/sample-nginx.log --format nginx -o examples/rawlog-nginx-replay.yaml
```

```csv title="examples/sample-nginx.csv (header + three rows)"
timestamp,severity,message,method,path,remote_addr,status,user_agent
1778832000,info,GET /api/v1/users HTTP/1.1 200,GET,/api/v1/users,192.168.1.10,200,Mozilla/5.0 (X11; Linux x86_64)
1778832004,warn,POST /api/v1/login HTTP/1.1 401,POST,/api/v1/login,192.168.1.12,401,Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)
1778832014,error,POST /api/v1/orders HTTP/1.1 500,POST,/api/v1/orders,192.168.1.17,500,kit/2.0
```

```yaml title="examples/rawlog-nginx-replay.yaml"
version: 2
defaults:
  rate: 1
  duration: 38s
  encoder:
    type: json_lines
  sink:
    type: stdout
scenarios:
- signal_type: logs
  name: sample_nginx_replay
  log_generator:
    type: csv_replay
    file: sample-nginx.csv
    default_severity: info
    repeat: false
```

The columns the NGINX format extracts:

| Column | Source | Notes |
|--------|--------|-------|
| `timestamp` | The bracketed `[DD/Mon/YYYY:HH:MM:SS +ZZZZ]` field | Converted to epoch seconds. Non-UTC offsets are normalized. |
| `severity` | The HTTP status code | See [the mapping table](#nginx-status-severity-mapping) below. |
| `message` | The request line + status (e.g. `GET /api/v1/users HTTP/1.1 200`) | |
| `method` | First token of the request line | `GET`, `POST`, etc. |
| `path` | Second token of the request line | |
| `remote_addr` | First whitespace-separated token of the line | Client IP. |
| `status` | The numeric status code, as a string | Preserved verbatim. |
| `user_agent` | The second quoted string after the status/bytes pair | Captured intact, including spaces. |

### NGINX status -> severity mapping

| Status | Severity |
|--------|----------|
| 1xx, 2xx, 3xx | `info` |
| 4xx | `warn` |
| 5xx | `error` |
| anything else | `info` (with a warn-level log noting the unrecognized status) |

Empty severity cells are not produced by this format -- every parseable line lands in one of the three buckets.

---

## Format: `plain`

`--format plain` is the fallback for unstructured text where every line is one record. The whole line becomes the `message`, the parser does not look for a timestamp or a severity, and synthesized timestamps are added so [`log_csv_replay`](log-csv-replay.md) has something to derive its rate from.

```text title="app.log"
starting service on port 8080
established db pool with 8 connections
worker thread 0 ready
worker thread 1 ready
incoming request from 10.0.0.42
```

```bash
sonda parsers rawlog app.log --format plain --delta-seconds 0.5 -o app-replay.yaml
```

```csv title="app.csv"
timestamp,severity,message
1700000000,,starting service on port 8080
1700000000.5,,established db pool with 8 connections
1700000001,,worker thread 0 ready
1700000001.5,,worker thread 1 ready
1700000002,,incoming request from 10.0.0.42
```

Three things to notice:

- **Synthesized timestamps start at `1700000000.0` epoch seconds** and increment by `--delta-seconds` (default `1.0`). That anchor is fixed and well-known so the CSVs are reproducible.
- **The severity cell is empty.** `log_csv_replay` reads `default_severity: info` from the emitted YAML and stamps every row with `Info` at replay time.
- **The CSV has no field columns** beyond `timestamp,severity,message`. Plain text has no structure to extract.

To replay faster than one event per second, pick a smaller `--delta-seconds`:

```bash
sonda parsers rawlog app.log --format plain --delta-seconds 0.1 -o burst.yaml
# emits at 10 events/s
```

---

## Running the result

Once the parser writes the YAML, it is an ordinary v2 scenario file. The same flags work as for any other `sonda run`:

```bash
# replay to stdout, default sink
sonda run --scenario examples/rawlog-nginx-replay.yaml

# replay to a Loki backend
sonda run --scenario examples/rawlog-nginx-replay.yaml \
  --sink loki --endpoint http://localhost:3100/loki/api/v1/push

# speed up by 10x via log_csv_replay's timescale -- edit the YAML to add `timescale: 10.0`
# under log_generator, then run normally
```

The emitted YAML's `rate: 1` is informational -- `log_csv_replay` derives the actual rate from the CSV's `timestamp` column. A warn line on startup tells you the derived value:

```text title="On startup, with --verbose"
WARN log_csv_replay 'sample_nginx_replay': overriding rate=1 with derived rate=0.5 samples/s (CSV Δt=2s, timescale=1)
```

For everything `log_csv_replay` can do -- `timescale`, severity fallback summaries, repeat behavior, explicit column mapping -- see the [Log CSV Replay](log-csv-replay.md) guide.

---

## Edge cases

The parser is strict about what it accepts and surfaces clear errors when input does not match the format:

| Situation | Result |
|-----------|--------|
| Empty file or only blank lines | Exits non-zero with `input file "<path>" contains no parseable rows`. |
| `--format` not in `plain` / `nginx` | Exits non-zero with `unknown format "<name>": must be one of ["plain", "nginx"]`. |
| `nginx` line that does not match the combined format | Skipped silently (the row is dropped). Mix valid and invalid lines in one file is fine; only the valid ones make it to the CSV. |
| `nginx` status outside known ranges (e.g. `999`) | Severity falls back to `info`; the parser emits one warn line per parse run noting the unrecognized status. |
| CSV cell contains commas or double-quotes | Quoted per RFC 4180. NGINX user-agent strings with commas are handled. |
| Blank line in the middle of the input | Skipped. The next non-blank line is parsed normally. |

---

## What is coming

Two formats today, more planned. The `LogFormatParser` trait keeps the door open for additional families:

- **`apache`** -- Apache combined log format (Common Log Format variant).
- **`json_lines`** -- NDJSON / JSON-per-line, with field discovery from the first object's keys.
- **`syslog`** -- RFC 3164 / RFC 5424 syslog framing.

Each is a fast-follow addition. Status lives on the [issue tracker](https://github.com/davidban77/sonda/issues).

For metric CSV reshaping or live API ingestion (Grafana, Loki, Prometheus), the planned `sonda-fetcher` companion will fill those roles.

---

## CLI reference

```
sonda parsers rawlog <FILE> --format <FORMAT> -o <OUTPUT> [OPTIONS]
```

See [CLI reference: `sonda parsers`](../configuration/cli-reference.md#sonda-parsers) for the full argument table and flag descriptions.
