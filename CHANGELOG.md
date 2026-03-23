# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.2.0](https://github.com/davidban77/sonda/compare/v0.1.3...v0.2.0) (2026-03-23)


### Features

* add CSV/file replay generator for production metric patterns ([#28](https://github.com/davidban77/sonda/issues/28)) ([a55c6ec](https://github.com/davidban77/sonda/commit/a55c6ecfa2fdb0800c3d0da2270c13895973c3dc))
* add phase_offset for multi-metric correlation ([#30](https://github.com/davidban77/sonda/issues/30)) ([4ed4c4e](https://github.com/davidban77/sonda/commit/4ed4c4ed2c1f3d5bd7eb5b7ec0909b0d001634f6))
* add Prometheus remote write encoder ([#27](https://github.com/davidban77/sonda/issues/27)) ([c7d7ec6](https://github.com/davidban77/sonda/commit/c7d7ec67cfc9843da89d9fb691f9e6e18e286f4d))
* add scrape endpoint GET /scenarios/{id}/metrics ([#25](https://github.com/davidban77/sonda/issues/25)) ([60dd1e8](https://github.com/davidban77/sonda/commit/60dd1e8c3469539e07e14e1de6f549d23af27897))
* add step/sequence value generator for incident pattern modeling ([#23](https://github.com/davidban77/sonda/issues/23)) ([0a0f928](https://github.com/davidban77/sonda/commit/0a0f928fc337408e602b54b6b4c6da6f0518990d))
* **slice-6.2:** add VictoriaMetrics compose stack and documentation ([#24](https://github.com/davidban77/sonda/issues/24)) ([739c337](https://github.com/davidban77/sonda/commit/739c3371fdc4c2afa9820569e31043ef06269c49))
* **slice-6.4:** add Alert Testing Guide for SRE adoption ([#26](https://github.com/davidban77/sonda/issues/26)) ([78116ef](https://github.com/davidban77/sonda/commit/78116ef51e3fc34e3b0a555740c696df53d7bfd7))
* **slice-7.2:** add pre-built Grafana dashboards and recording rule example ([#29](https://github.com/davidban77/sonda/issues/29)) ([34c4e79](https://github.com/davidban77/sonda/commit/34c4e79819657da0fa26f7221079c5bb76dcb977))


### Documentation

* add multi-metric correlation section to alert testing guide ([#31](https://github.com/davidban77/sonda/issues/31)) ([4341924](https://github.com/davidban77/sonda/commit/4341924f8befda5deacd6f57ac81d8156162b75a))
* add Phase 6 and Phase 7 plans ([#20](https://github.com/davidban77/sonda/issues/20)) ([56f4da0](https://github.com/davidban77/sonda/commit/56f4da02d1ba394176a2e79e74371044efda3d89))
* fix README accuracy and documentation drift ([#22](https://github.com/davidban77/sonda/issues/22)) ([294b680](https://github.com/davidban77/sonda/commit/294b680a8449c5d18431ec144ff8f7e7a903f4c3))
* improve Phase 8 plan and docs agent with workflow integration ([#32](https://github.com/davidban77/sonda/issues/32)) ([f85ffa4](https://github.com/davidban77/sonda/commit/f85ffa4c2429758ea4fad2a74910ce1b36ba97ed))
* **slice-8.0:** MkDocs scaffold and landing page ([#33](https://github.com/davidban77/sonda/issues/33)) ([a518c27](https://github.com/davidban77/sonda/commit/a518c27d23541dac1f06195e753e3407ead9874e))
* **slice-8.1:** add MkDocs scaffold and getting started guide ([#34](https://github.com/davidban77/sonda/issues/34)) ([0066cb7](https://github.com/davidban77/sonda/commit/0066cb77a91ab86d53cfb6e58c3400af85bde608))
* **slice-8.2:** add configuration reference pages ([#35](https://github.com/davidban77/sonda/issues/35)) ([3bdd64d](https://github.com/davidban77/sonda/commit/3bdd64d59f758aa60d14e3c4427367b339ee9d83))
* **slice-8.3:** add alert testing guide ([#36](https://github.com/davidban77/sonda/issues/36)) ([15f80a9](https://github.com/davidban77/sonda/commit/15f80a9b84a64ea48d91dd516cb4302b05fe867f))
* **slice-8.4:** add pipeline validation and recording rules guides ([#37](https://github.com/davidban77/sonda/issues/37)) ([9e3246e](https://github.com/davidban77/sonda/commit/9e3246efc1fcb488fc7747fc506cf56c82d0c8ce))

## [0.1.3](https://github.com/davidban77/sonda/compare/v0.1.2...v0.1.3) (2026-03-22)


### Bug Fixes

* **ci:** only dry-run sonda-core in publish workflow ([#18](https://github.com/davidban77/sonda/issues/18)) ([8c68b72](https://github.com/davidban77/sonda/commit/8c68b72f526c9e1aec30da8ee6e5761b02a7ed7a))

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
