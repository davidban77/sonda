# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

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
