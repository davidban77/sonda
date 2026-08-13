---
title: Editor Schema
description: Point your editor at Sonda's JSON Schema for completion and inline validation of scenario YAML.
---

# Editor Schema

Sonda publishes a [JSON Schema](../schema/sonda-scenario.schema.json) for the v2 scenario file
format. Point your editor at it and you get key completion, hover documentation pulled from the
Rust doc comments, and a red underline on a misspelled field — before you run anything.

The schema lives at:

```text
https://davidban77.github.io/sonda/schema/sonda-scenario.schema.json
```

## Per-file, no configuration

The quickest way, and the one that travels with the file. Add a modeline comment as the first
line of any scenario:

```yaml title="cpu.yaml"
# yaml-language-server: $schema=https://davidban77.github.io/sonda/schema/sonda-scenario.schema.json
version: 2
kind: runnable

scenarios:
  - signal_type: metrics
    name: cpu_usage
    rate: 10
    duration: 60s
    generator:
      type: sine
      amplitude: 50.0
      period_secs: 30
      offset: 50.0
```

The comment is understood by the YAML language server that backs the VS Code
[YAML extension](https://marketplace.visualstudio.com/items?itemName=redhat.vscode-yaml), Neovim's
`yamlls`, Helix, and Zed. Sonda ignores it — it is a YAML comment.

## Per-project, by filename pattern

If your scenarios follow a naming convention, wire the schema up once instead of per file.

=== "VS Code"

    In `.vscode/settings.json`:

    ```json
    {
      "yaml.schemas": {
        "https://davidban77.github.io/sonda/schema/sonda-scenario.schema.json": [
          "scenarios/**/*.yaml",
          "*.sonda.yaml"
        ]
      }
    }
    ```

=== "Neovim (yamlls)"

    ```lua
    require("lspconfig").yamlls.setup({
      settings = {
        yaml = {
          schemas = {
            ["https://davidban77.github.io/sonda/schema/sonda-scenario.schema.json"] = {
              "scenarios/**/*.yaml",
              "*.sonda.yaml",
            },
          },
        },
      },
    })
    ```

=== "Offline"

    The schema is a plain file. Vendor it and point at the local copy:

    ```console
    $ curl -sSLO https://davidban77.github.io/sonda/schema/sonda-scenario.schema.json
    ```

    Or generate it from the source you are building against, which is what the repository's own
    copy is produced by:

    ```console
    $ cargo run -p sonda-core --all-features --example scenario_schema > sonda.schema.json
    ```

    `--all-features` matters: the optional delivery features add config shape, so a narrower
    build emits a schema that rejects sink config a release binary accepts.

## What the schema checks, and what it does not

The schema is an authoring aid. `sonda` itself remains the validator, and it enforces rules that
JSON Schema cannot express. A file your editor is happy with can still be rejected when you run
it.

| Checked by the schema | Checked only by `sonda` |
| --- | --- |
| Unknown or misspelled field names | `id` uniqueness across entries |
| Unknown `kind:`, generator `type:`, encoder `type:`, sink `type:` | `after.ref` and `while.ref` pointing at an entry that exists |
| Structural shape — `scenarios:` is a list, `labels:` is a mapping | `delay:` requiring a `while:` on the same entry |
| The `while.op` operator being one of `<` or `>` | `generator:` and `pack:` being mutually exclusive |
| Required fields being present | Value ranges (encoder `precision` is 0–17, `max_attempts` ≥ 1) |
| The two shapes `delay.close:` accepts | Dependency cycles in `after:` chains |

Run `sonda validate <file>` for the full check.

### Scalar types are deliberately loose

You will notice the schema accepts a number or a boolean anywhere it accepts a string — so
`status: 200` under `labels:` is not flagged, even though a label value is a string.

That is on purpose, and it matches the parser. Sonda's YAML loader coerces any plain scalar into
a string field, so `status: 200` becomes the label value `"200"` and the file runs. A schema that
insisted on `"type": "string"` would underline working scenarios in red — including several in
this repository's own `examples/` directory. Over-rejection is the one failure that makes a schema
worse than no schema, so the schema follows what Sonda accepts rather than what the Rust types
declare.

## Keeping up with your version

The published schema tracks the latest release. If you pin an older `sonda`, generate the schema
from that version rather than fetching the published one — the `--example scenario_schema`
invocation above works from any checkout or crate source.

The schema is generated from the same Rust types the parser deserializes into, and the repository
holds a test that fails if the committed copy falls behind them, plus a corpus check that every
scenario under `examples/` still validates. It cannot quietly describe an older format.
