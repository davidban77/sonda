# Run it in CI with the GitHub Action

`sonda test` turns alert rules into a test suite. The action turns that into
three lines of workflow YAML — it installs a pinned Sonda release, verifies
the download against the release's published checksums, and runs your
scenario. The exit code is the check.

```yaml title=".github/workflows/alert-rules.yml"
name: Alert rules
on: [pull_request]

jobs:
  verify:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      # ... start your Prometheus/vmalert stack here ...
      - uses: davidban77/sonda@v1
        with:
          scenario: examples/alert-lifecycle-test.yaml
          prometheus-url: http://localhost:8428
```

The job fails when an alert does not fire, or does not resolve, on deadline
— the same contract `sonda test` has on your laptop.

## Inputs

| Input | Required | Description |
|---|---|---|
| `scenario` | yes | Path to a v2 scenario YAML with an `expect:` block, relative to the workspace. |
| `prometheus-url` | one of | Base URL of the Prometheus-compatible API evaluating the rules — verifies the **rule** fires. |
| `alertmanager-url` | one of | Base URL of the Alertmanager receiving the alerts — verifies the **notification** arrived. |
| `version` | no | Release to install (e.g. `v1.20.0`), or `latest`. Defaults to the ref you pinned the action at. |
| `extra-args` | no | Extra arguments for `sonda test`, split on whitespace (e.g. `--interval 5s`). |

**Exactly one of `prometheus-url` / `alertmanager-url`**, mirroring the CLI.
Passing both is an error: they verify different hops of the alerting path,
so there is no sensible way to merge their verdicts. To cover both, add two
steps — and the one that fails tells you which hop broke.

!!! note "`alertmanager-url` has a version floor"
    That input needs a Sonda release that has it. If you pin an older
    version, the action refuses **before** downloading anything and names
    the floor, rather than installing a binary that would reject the flag
    with a confusing `unexpected argument`.

## What the action does

1. **Resolves your ref to a concrete release.** `@v1` is a tag that moves
   with every release, so it is resolved to the newest published `1.x`
   before anything is downloaded — a run always installs a determinate
   version, and the log says which.
2. **Installs that release**, reusing the same `install.sh` the docs point
   humans at: it maps the runner's OS and architecture to the release
   asset, downloads it, and **verifies it against the release's
   `SHA256SUMS`**. A mismatch aborts.
3. **Runs `sonda test`** with your scenario and URL.

Two failure modes get named rather than left to guesswork:

- **A tag whose release has no assets yet.** The version bump merges and
  tags before the binary build finishes uploading, so for a few minutes a
  real tag has nothing to download. The action says so and suggests
  re-running or pinning the previous version.
- **A release with assets but no checksums.** The action refuses instead of
  installing something it cannot verify.

## Pinning

```yaml
- uses: davidban77/sonda@v1        # newest 1.x, moves with each release
- uses: davidban77/sonda@v1.20.0   # exactly this release, forever
```

`@v1` is maintained by the release workflow, which moves the `v1` tag onto
each new `1.x` release. A future `2.0.0` creates and moves `v2` and leaves
`v1` where it is, so pinning `@v1` never carries you across a major version.

## Verifying the notification path

Swap the flag to check the hop past the rule evaluator — did the alert
actually reach Alertmanager?

```yaml
- uses: davidban77/sonda@v1
  with:
    scenario: examples/alert-lifecycle-test.yaml
    alertmanager-url: http://localhost:9093
```

A rule that evaluates correctly but never reaches Alertmanager — a wrong
`-notifier.url`, a route that drops it — is green on `prometheus-url` and
red here. See [Alert testing](alert-testing.md#verify-the-notification-not-just-the-rule)
for what that path can and cannot promise.

## Where to next

- [Alert testing](alert-testing.md) — writing the `expect:` block itself.
- [End-to-end pipelines](end-to-end-pipelines.md) — the full CI walkthrough,
  including bringing up the backend stack the action runs against.
