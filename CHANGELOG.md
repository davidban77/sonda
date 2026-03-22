# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.1.2](https://github.com/davidban77/sonda/compare/v0.1.1...v0.1.2) (2026-03-22)


### Bug Fixes

* **ci:** add deps to allowed commit types and ignore major dep bumps ([#11](https://github.com/davidban77/sonda/issues/11)) ([a5a2cee](https://github.com/davidban77/sonda/commit/a5a2cee6dc12f58d0a25a6879d9d9785d05121a9))
* **ci:** use PAT for release-please to trigger release workflow ([48158a9](https://github.com/davidban77/sonda/commit/48158a96eddef9861bf1a12b4eebc2b644d8fc5a))


### Miscellaneous

* upgrade axum 0.8, tower-http 0.6, reqwest 0.13, and GHA versions ([#17](https://github.com/davidban77/sonda/issues/17)) ([06ac1d8](https://github.com/davidban77/sonda/commit/06ac1d8416a779d0a4080c1e5cddbe56826a50ea))


### CI/CD

* bump actions/download-artifact from 4 to 8 ([#5](https://github.com/davidban77/sonda/issues/5)) ([0eaf348](https://github.com/davidban77/sonda/commit/0eaf348e209875d0ae9c1a4916f3e317de79d720))
* bump amannn/action-semantic-pull-request from 5 to 6 ([#1](https://github.com/davidban77/sonda/issues/1)) ([0384d17](https://github.com/davidban77/sonda/commit/0384d1715b8c30222a174ad4ad97503e2fd11c9b))
* bump docker/build-push-action from 6 to 7 ([#3](https://github.com/davidban77/sonda/issues/3)) ([e872975](https://github.com/davidban77/sonda/commit/e872975191bbfb9c18207e99215195292a08eed2))
* bump docker/metadata-action from 5 to 6 ([#2](https://github.com/davidban77/sonda/issues/2)) ([b47759f](https://github.com/davidban77/sonda/commit/b47759f12c680ea2b8d45373a4db3daad3400900))
* bump docker/setup-qemu-action from 3 to 4 ([#4](https://github.com/davidban77/sonda/issues/4)) ([e00b628](https://github.com/davidban77/sonda/commit/e00b628573d6ceb214ccc5b1d363a274fb7113ea))

## [0.1.1](https://github.com/davidban77/sonda/compare/v0.1.0...v0.1.1) (2026-03-22)


### Bug Fixes

* **ci:** switch release-please to simple type for cargo workspace ([976e042](https://github.com/davidban77/sonda/commit/976e0429670f95a8eef47aad288c9e0ad8081046))
* **docker:** replace musl.cc download with apt gcc-aarch64-linux-gnu ([901e450](https://github.com/davidban77/sonda/commit/901e450cc55cb53443da3afd4d1619f494a72191))
* use explicit version in crate Cargo.toml for release-please ([fadbade](https://github.com/davidban77/sonda/commit/fadbade7fb487570634a9a23ea1e012a99820fa0))


### CI/CD

* trigger release-please after enabling PR permissions ([d85b5c3](https://github.com/davidban77/sonda/commit/d85b5c32e4b172d49f4d5dfd8315fc0332a014ae))

## [Unreleased]

## [0.1.0]

Phases 0, 1, and 2 are complete. The following major capabilities are available:

### Added

- **Metrics generation** — configurable value generators (sine, sawtooth, constant,
  uniform random, step, pulse) with per-metric label sets and configurable scrape intervals.
- **Log generation** — structured log event generation with configurable level distributions,
  message templates, and label sets.
- **Burst and gap windows** — first-class burst and gap scheduling: emit nothing during gaps,
  emit at elevated rates during bursts.
- **Multi-scenario concurrency** — run multiple independent scenarios in parallel using OS
  threads with a shared channel sink for coordinated output.
- **Encoders** — Prometheus text exposition format, InfluxDB Line Protocol, JSON Lines, syslog
  (RFC 5424).
- **Sinks** — stdout, file, TCP, UDP, HTTP (remote-write compatible), Kafka, Loki.
- **CLI** — `sonda run` and `sonda logs` subcommands with YAML scenario config and `SONDA_*`
  environment variable overrides.
- **Static binary** — statically linked musl target for portable, zero-dependency deployment.
