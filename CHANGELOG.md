# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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
