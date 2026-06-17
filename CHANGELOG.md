# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [1.17.1](https://github.com/davidban77/sonda/compare/v1.17.0...v1.17.1) (2026-06-17)


### Bug Fixes

* **ci:** pin dtolnay/rust-toolchain to 1.95.0 + add drift guard ([#480](https://github.com/davidban77/sonda/issues/480)) ([e37289a](https://github.com/davidban77/sonda/commit/e37289acedbf47fd143f68e7ae0f7751c6fe3d30))


### CI/CD

* bump mendhak/http-https-echo from 40 to 41 in /examples ([#482](https://github.com/davidban77/sonda/issues/482)) ([5ba2387](https://github.com/davidban77/sonda/commit/5ba238794505d715d22b14e67fcd53f4dca50003))
* bump otel/opentelemetry-collector-contrib in /examples ([#484](https://github.com/davidban77/sonda/issues/484)) ([a37eabb](https://github.com/davidban77/sonda/commit/a37eabb74bc8e4cd7ef91f1fff8c723899e0e697))
* bump prom/alertmanager from v0.32.1 to v0.33.0 in /examples ([#486](https://github.com/davidban77/sonda/issues/486)) ([590e1e3](https://github.com/davidban77/sonda/commit/590e1e31d8f0327420800b548689db51e5d84a77))
* bump victoriametrics/vmagent from v1.144.0 to v1.145.0 in /examples ([#485](https://github.com/davidban77/sonda/issues/485)) ([6fae049](https://github.com/davidban77/sonda/commit/6fae049a50366e101b3d5c7aa0ff2339eab3cc64))
* bump victoriametrics/vmalert from v1.144.0 to v1.145.0 in /examples ([#483](https://github.com/davidban77/sonda/issues/483)) ([4e317be](https://github.com/davidban77/sonda/commit/4e317be7c351a95cc0c806a6c7472c730eb697e6))

## [1.17.0](https://github.com/davidban77/sonda/compare/v1.16.1...v1.17.0) (2026-06-16)


### Features

* **server:** sonda-server runs as non-root UID 65532 by default ([#475](https://github.com/davidban77/sonda/issues/475)) ([9917c6b](https://github.com/davidban77/sonda/commit/9917c6ba86abd5d70643b5c852a764a8cccb5bb1))


### Documentation

* **readme:** switch crate-version + license badges away from broken shields.io endpoints ([#479](https://github.com/davidban77/sonda/issues/479)) ([3e433d1](https://github.com/davidban77/sonda/commit/3e433d1d3b969e4ac6e02862bb3cefe3b76fc712))
* surface flag descriptions in sonda run --help, self-contain pack example, document workers cap ([#477](https://github.com/davidban77/sonda/issues/477)) ([fcf309d](https://github.com/davidban77/sonda/commit/fcf309d23b4da7c9dd29050d230976a9fdd49499))


### CI/CD

* bump dtolnay/rust-toolchain from 1.95.0 to 1.100.0 ([#443](https://github.com/davidban77/sonda/issues/443)) ([8405197](https://github.com/davidban77/sonda/commit/8405197f2ad57023334a69a86674ddb22be7ff97))

## [1.16.1](https://github.com/davidban77/sonda/compare/v1.16.0...v1.16.1) (2026-06-16)


### Bug Fixes

* **server:** recover from poisoned RwLock in request-metrics middleware ([#473](https://github.com/davidban77/sonda/issues/473)) ([fa2daeb](https://github.com/davidban77/sonda/commit/fa2daebf67e2a1906cc47c463b961043dca74c31))


### CI/CD

* bump otel/opentelemetry-collector-contrib in /examples ([#451](https://github.com/davidban77/sonda/issues/451)) ([943f271](https://github.com/davidban77/sonda/commit/943f27133cb914b92e2bf117ad100ec9f8c48fd0))
* bump prom/prometheus from v3.11.3 to v3.12.0 in /examples ([#450](https://github.com/davidban77/sonda/issues/450)) ([e324d9d](https://github.com/davidban77/sonda/commit/e324d9d2f8c97778a4cfdc381e63dbd4ed4c24b2))
* bump victoriametrics/victoria-metrics in /examples ([#452](https://github.com/davidban77/sonda/issues/452)) ([9f394dd](https://github.com/davidban77/sonda/commit/9f394ddfe2ec6598f0e15a0abdb2d4c388cc6c3b))
* bump victoriametrics/vmagent from v1.143.0 to v1.144.0 in /examples ([#448](https://github.com/davidban77/sonda/issues/448)) ([ae14a59](https://github.com/davidban77/sonda/commit/ae14a593fee74521825df41cd2a90aa3b14885dd))
* bump victoriametrics/vmalert from v1.143.0 to v1.144.0 in /examples ([#449](https://github.com/davidban77/sonda/issues/449)) ([22b5020](https://github.com/davidban77/sonda/commit/22b5020b17c05a6516a8eeb8c3258122ce17c42a))

## [1.16.0](https://github.com/davidban77/sonda/compare/v1.15.0...v1.16.0) (2026-06-15)


### Features

* **server:** align metrics routes with Prometheus convention ([#468](https://github.com/davidban77/sonda/issues/468)) ([a73b574](https://github.com/davidban77/sonda/commit/a73b574d296ed18530af035bfd302ce0a3cf8e81))


### Refactoring

* **core:** gate_bus Notify + AtomicU8 primitive ([#471](https://github.com/davidban77/sonda/issues/471)) ([6baf20c](https://github.com/davidban77/sonda/commit/6baf20c6e0d9d9bc58a74b7aa792ca03086c3bf0))
* **server:** cleanup_scenarios test helper uses join_async ([#472](https://github.com/davidban77/sonda/issues/472)) ([ed8023a](https://github.com/davidban77/sonda/commit/ed8023a26aa5cd61994ab4f5a75bb8f0e920cc29))

## [1.15.0](https://github.com/davidban77/sonda/compare/v1.14.0...v1.15.0) (2026-06-15)


### Features

* async-native scheduler — bounded operator surface + /server/metrics ([#467](https://github.com/davidban77/sonda/issues/467)) ([910c105](https://github.com/davidban77/sonda/commit/910c105301f27061678bc4e03a13aaae58cad764))


### Documentation

* **site:** reorganize for newcomers — outcome-named IA, tabs, glossary ([#442](https://github.com/davidban77/sonda/issues/442)) ([716bb5b](https://github.com/davidban77/sonda/commit/716bb5b0f304997483368421cf29135a7730e91b))

## [1.14.0](https://github.com/davidban77/sonda/compare/v1.13.1...v1.14.0) (2026-05-30)


### Features

* **server:** add ScenarioState::Held + ?include_state=held filter token ([#440](https://github.com/davidban77/sonda/issues/440)) ([a7fc4c9](https://github.com/davidban77/sonda/commit/a7fc4c90918780e3a5cac890e0946e0d3e40d90d))

## [1.13.1](https://github.com/davidban77/sonda/compare/v1.13.0...v1.13.1) (2026-05-27)


### Bug Fixes

* **server:** add /metrics?include_state= allowlist filter ([#435](https://github.com/davidban77/sonda/issues/435)) ([d74514a](https://github.com/davidban77/sonda/commit/d74514aa7e94cd0b27f15d4057193ecf0c436091))


### Documentation

* clarify cross-POST while: ghost samples, validate=strict, 409 collisions ([#430](https://github.com/davidban77/sonda/issues/430)) ([072506c](https://github.com/davidban77/sonda/commit/072506c61354ff4b6b79ea9162f5a741d2c4df2f))
* label cross-POST while: patterns A/B and add include_state + if_unresolved callout ([#438](https://github.com/davidban77/sonda/issues/438)) ([c884138](https://github.com/davidban77/sonda/commit/c884138114c50fad4dd7164dfadb8b93e973980b))

## [1.13.0](https://github.com/davidban77/sonda/compare/v1.12.2...v1.13.0) (2026-05-26)


### Features

* cross-POST while: ref resolution across scenario sources ([#426](https://github.com/davidban77/sonda/issues/426)) ([6a0b673](https://github.com/davidban77/sonda/commit/6a0b6732b6d8dd791f3e71b31c2aba3ef2d25ce1))


### Bug Fixes

* **ci:** pin release workflow to dtolnay/rust-toolchain@1.95.0 ([#416](https://github.com/davidban77/sonda/issues/416)) ([2dadf53](https://github.com/davidban77/sonda/commit/2dadf53212ae473398a7b39e37fc8583c4bef7c0))
* **core:** finish local-POST while-gated downstreams on upstream exit ([#428](https://github.com/davidban77/sonda/issues/428)) ([4923172](https://github.com/davidban77/sonda/commit/4923172c42bc2d51645dbfd2d054d35c094272c6))
* **server:** isolate scenario shutdown flag so DELETE does not cascade ([#420](https://github.com/davidban77/sonda/issues/420)) ([365658a](https://github.com/davidban77/sonda/commit/365658a7a66264d1c13a728a84cb38214b6f5d61))


### Documentation

* minimize README, defer to docs site for tutorials and references ([#415](https://github.com/davidban77/sonda/issues/415)) ([6dabb96](https://github.com/davidban77/sonda/commit/6dabb96a333b6744a68ae5778922f8c3d2438675))

## [1.12.2](https://github.com/davidban77/sonda/compare/v1.12.1...v1.12.2) (2026-05-25)


### Bug Fixes

* **server:** emit one Prometheus sample per series with no timestamp ([#414](https://github.com/davidban77/sonda/issues/414)) ([c41b290](https://github.com/davidban77/sonda/commit/c41b29041f8905e0612ef1bfe3ccd04aba606007))


### Documentation

* rewrite concepts guide for newcomer onramp ([#412](https://github.com/davidban77/sonda/issues/412)) ([f1c9dcd](https://github.com/davidban77/sonda/commit/f1c9dcdc81802921290d88fb627b9de9ce7a25b2))

## [1.12.1](https://github.com/davidban77/sonda/compare/v1.12.0...v1.12.1) (2026-05-24)


### Bug Fixes

* **server:** emit # HELP and # TYPE annotations on /metrics endpoints ([#411](https://github.com/davidban77/sonda/issues/411)) ([7605d69](https://github.com/davidban77/sonda/commit/7605d693964976ef3785a70e7820696ecdee8a2e))


### CI/CD

* bump grafana/loki from 3.7.1 to 3.7.2 in /examples ([#402](https://github.com/davidban77/sonda/issues/402)) ([70f94a8](https://github.com/davidban77/sonda/commit/70f94a886a3ea66dfc2989c4de24ca8e078b8c41))
* bump otel/opentelemetry-collector-contrib in /examples ([#406](https://github.com/davidban77/sonda/issues/406)) ([50a846a](https://github.com/davidban77/sonda/commit/50a846a2cac2f0b409f4cabdc16add26eae2edd7))
* bump victoriametrics/victoria-metrics in /examples ([#407](https://github.com/davidban77/sonda/issues/407)) ([0f6d9e8](https://github.com/davidban77/sonda/commit/0f6d9e8da3077b58ed0cee418ddde9d2ba0ce1a9))
* bump victoriametrics/vmagent from v1.142.0 to v1.143.0 in /examples ([#409](https://github.com/davidban77/sonda/issues/409)) ([d098643](https://github.com/davidban77/sonda/commit/d098643f3f5286b1693c720f97f62147476207f2))
* bump victoriametrics/vmalert from v1.142.0 to v1.143.0 in /examples ([#408](https://github.com/davidban77/sonda/issues/408)) ([2f1e4bb](https://github.com/davidban77/sonda/commit/2f1e4bb9e212d0fbc9f6b916af0ccfd7f0d6564d))

## [1.12.0](https://github.com/davidban77/sonda/compare/v1.11.0...v1.12.0) (2026-05-24)


### Features

* **server:** aggregate /metrics endpoint with label filter ([#401](https://github.com/davidban77/sonda/issues/401)) ([7e7ef77](https://github.com/davidban77/sonda/commit/7e7ef77b189fe51167491374ee58c917544a7cc9))


### Documentation

* correct the metric-pack file shape to flat top-level fields ([#398](https://github.com/davidban77/sonda/issues/398)) ([fc315e6](https://github.com/davidban77/sonda/commit/fc315e64651c20600ee5b5c7b40c5a32231f20f9))
* introduce concepts page and drop v2 framing from scenario docs ([#400](https://github.com/davidban77/sonda/issues/400)) ([ea67f3a](https://github.com/davidban77/sonda/commit/ea67f3a01c6daf2e9f7ded4da2856efdaf059555))

## [1.11.0](https://github.com/davidban77/sonda/compare/v1.10.2...v1.11.0) (2026-05-22)


### Features

* **core:** add start_time for time-shifted emission ([#396](https://github.com/davidban77/sonda/issues/396)) ([5900c63](https://github.com/davidban77/sonda/commit/5900c639ca91a4cf60b8fc0361e26c064f706aa4))
* **server:** resolve pack: references in posted scenarios via --catalog ([#394](https://github.com/davidban77/sonda/issues/394)) ([8ae856b](https://github.com/davidban77/sonda/commit/8ae856bda29f881707f695276e1dcb6b14ac7cd6))


### Documentation

* add newcomer on-ramps to the deployment pages ([#397](https://github.com/davidban77/sonda/issues/397)) ([488085d](https://github.com/davidban77/sonda/commit/488085d640efe3e19ade7fed69ffbc5eaa1c56fe))

## [1.10.2](https://github.com/davidban77/sonda/compare/v1.10.1...v1.10.2) (2026-05-20)


### Bug Fixes

* **core:** require duration on after:-gated scenarios ([#393](https://github.com/davidban77/sonda/issues/393)) ([3517db4](https://github.com/davidban77/sonda/commit/3517db4d2c2c95932c2bccfceec851140b45cda4))


### Miscellaneous

* clear clippy --all-targets lint debt and gate it in CI ([#390](https://github.com/davidban77/sonda/issues/390)) ([e15f4c0](https://github.com/davidban77/sonda/commit/e15f4c0937714972fb6a77c92eca8c2fd79e3b73))


### Refactoring

* **core:** build the scenario encoder once and share it with close-emit ([#392](https://github.com/davidban77/sonda/issues/392)) ([ee16a08](https://github.com/davidban77/sonda/commit/ee16a08fb8988320e7d07577a578ca72e4e10f14))

## [1.10.1](https://github.com/davidban77/sonda/compare/v1.10.0...v1.10.1) (2026-05-19)


### Bug Fixes

* **core:** emit a close marker for every distinct series, not just the most recent 100 ([#387](https://github.com/davidban77/sonda/issues/387)) ([703ad60](https://github.com/davidban77/sonda/commit/703ad60eba91e9a37b805555c680bb16210be327))

## [1.10.0](https://github.com/davidban77/sonda/compare/v1.9.3...v1.10.0) (2026-05-19)


### Features

* **core:** mark FlapEnum and CloseSignal as #[non_exhaustive] ([#385](https://github.com/davidban77/sonda/issues/385)) ([ed4fd85](https://github.com/davidban77/sonda/commit/ed4fd859cfc6fd2a31e8491cfe920792f48069d0))
* **core:** reject stale_marker on non-remote_write sinks; surface snap_to and stale_marker in --dry-run ([#386](https://github.com/davidban77/sonda/issues/386)) ([a1bf3f8](https://github.com/davidban77/sonda/commit/a1bf3f859cfe431253030a95f0f1486333169427))
* **server:** add degraded field to scenario detail and stats endpoints ([#383](https://github.com/davidban77/sonda/issues/383)) ([9cefb2b](https://github.com/davidban77/sonda/commit/9cefb2b614f42632cc60aee802e3955ed30c214d))


### Bug Fixes

* **config:** validate sink duration fields in --dry-run ([#381](https://github.com/davidban77/sonda/issues/381)) ([25378ce](https://github.com/davidban77/sonda/commit/25378ce74f629a1a61000eb49f96e18ac0d0957c))
* **helm:** require explicit ServiceMonitor path; document per-scenario metrics ([#384](https://github.com/davidban77/sonda/issues/384)) ([c257bce](https://github.com/davidban77/sonda/commit/c257bce064b50c0a98097ac94f788f010c0fc66b))

## [1.9.3](https://github.com/davidban77/sonda/compare/v1.9.2...v1.9.3) (2026-05-19)


### Bug Fixes

* **loki:** address [@reviewer](https://github.com/reviewer) findings on 1.9.4 cumulative diff ([#378](https://github.com/davidban77/sonda/issues/378)) ([af69b20](https://github.com/davidban77/sonda/commit/af69b205189756f652dc9ad85e2444fa044e282e))
* **loki:** auto-promote dynamic_labels to Loki streams + cardinality cap (PR 2/4 for [#296](https://github.com/davidban77/sonda/issues/296)) ([#376](https://github.com/davidban77/sonda/issues/376)) ([b6e0111](https://github.com/davidban77/sonda/commit/b6e0111dcf7c375b0cc072fa1beb50b498ea67ab))
* **server:** loki cardinality preview + docs sweep (PR 3/4 for [#296](https://github.com/davidban77/sonda/issues/296)) ([#377](https://github.com/davidban77/sonda/issues/377)) ([faeb9b9](https://github.com/davidban77/sonda/commit/faeb9b9af8c7f110bffeb06c785551ff90df0c74))
* **sink:** add Sink::write_log_event with bytes-only default impl ([#375](https://github.com/davidban77/sonda/issues/375)) ([0189bf6](https://github.com/davidban77/sonda/commit/0189bf61d48e9d9dee838816e393e91d84cfd543))


### Documentation

* **theme:** mobile hero polish + hide TOC permalink on hero titles ([#373](https://github.com/davidban77/sonda/issues/373)) ([c176e3f](https://github.com/davidban77/sonda/commit/c176e3f44fb433ec6acae9363c05867201b45ecb))

## [1.9.2](https://github.com/davidban77/sonda/compare/v1.9.1...v1.9.2) (2026-05-18)


### CI/CD

* bump dtolnay/rust-toolchain from 1.95.0 to 1.100.0 ([#302](https://github.com/davidban77/sonda/issues/302)) ([b9623b3](https://github.com/davidban77/sonda/commit/b9623b3abf95a1388d6dcb2c5aefea929815f9c1))
* bump otel/opentelemetry-collector-contrib in /examples ([#306](https://github.com/davidban77/sonda/issues/306)) ([b502513](https://github.com/davidban77/sonda/commit/b5025135d6617a2fb8d0199eb7eaf64c006361bc))
* bump prom/alertmanager from v0.32.0 to v0.32.1 in /examples ([#301](https://github.com/davidban77/sonda/issues/301)) ([4761ceb](https://github.com/davidban77/sonda/commit/4761cebb3f1e024c149750c217a40d3c907078d5))
* bump prom/prometheus from v3.11.2 to v3.11.3 in /examples ([#305](https://github.com/davidban77/sonda/issues/305)) ([c031e4b](https://github.com/davidban77/sonda/commit/c031e4b532473110aa6f7d04862f293a76f1d6dd))
* bump victoriametrics/vmagent from v1.140.0 to v1.142.0 in /examples ([#304](https://github.com/davidban77/sonda/issues/304)) ([b3121d8](https://github.com/davidban77/sonda/commit/b3121d8b47647b4f899e0ba9c90e766cf7bde643))
* bump victoriametrics/vmalert from v1.140.0 to v1.142.0 in /examples ([#303](https://github.com/davidban77/sonda/issues/303)) ([1d5e028](https://github.com/davidban77/sonda/commit/1d5e028f2a2731ba3479669802f6b13d04d2de10))

## [1.9.1](https://github.com/davidban77/sonda/compare/v1.9.0...v1.9.1) (2026-05-18)


### Features

* **docs:** switch theme from Probe (lime) to Sunrise (coral) + warm dark-mode code surfaces ([#369](https://github.com/davidban77/sonda/issues/369)) ([d8ed9b6](https://github.com/davidban77/sonda/commit/d8ed9b615a28ca08f2db1281ae79fa95933acc8b))


### Documentation

* **deployment:** correct --catalog position for Docker invocations; document the dispatch shim ([#367](https://github.com/davidban77/sonda/issues/367)) ([7ef0342](https://github.com/davidban77/sonda/commit/7ef034274fe6db2342006587948cc7a110372e07))
* modernize site with Probe palette + sine-wave S branding ([#364](https://github.com/davidban77/sonda/issues/364)) ([7915c43](https://github.com/davidban77/sonda/commit/7915c43e5e0745e8ad2ed421b27bb0b831287d12))
* **scenario-fields:** add Prometheus/Grafana verification guide to §Gap window ([#366](https://github.com/davidban77/sonda/issues/366)) ([2a48555](https://github.com/davidban77/sonda/commit/2a48555c7629fa45e342462fae44220ed64054bb))


### Miscellaneous

* **e2e:** migrate fixtures to v2 + update runner for the post-1.9 CLI ([#368](https://github.com/davidban77/sonda/issues/368)) ([dc1d830](https://github.com/davidban77/sonda/commit/dc1d8304db90bbf84c29da11a19bb10a5bd0e1eb))
* release as 1.9.1 ([#370](https://github.com/davidban77/sonda/issues/370)) ([63ea6af](https://github.com/davidban77/sonda/commit/63ea6afbc9a4e4afdee1883efc4a543b92a02bd5))
* release-please override → 1.9.1 ([f4be633](https://github.com/davidban77/sonda/commit/f4be63337768dfba75aace2b56dce9e178ab67e0))

## [1.9.0](https://github.com/davidban77/sonda/compare/v1.8.0...v1.9.0) (2026-05-16)


### Features

* **cli:** 1.9c — collapse to 4 verbs; mandatory --catalog; sonda new replaces init ([#356](https://github.com/davidban77/sonda/issues/356)) ([9570233](https://github.com/davidban77/sonda/commit/957023345d936ade817eb1e328bbbca965c6fdc9))
* **compiler:** require kind: frontmatter on v2 YAML; add optional tags: ([#353](https://github.com/davidban77/sonda/issues/353)) ([ade5f88](https://github.com/davidban77/sonda/commit/ade5f8807976fa41d72b68b020e6c641464efe82))
* **log-csv-replay:** structured-CSV log replay with derived rate, severity column, and field columns ([#349](https://github.com/davidban77/sonda/issues/349)) ([a2e75b5](https://github.com/davidban77/sonda/commit/a2e75b539a91866a9fb0cd31c556962dc076d768))
* **server:** drop SONDA_PACK_PATH catalog; trim SONDA_SUBCOMMANDS to 4 verbs (1.9d) ([#357](https://github.com/davidban77/sonda/issues/357)) ([0e82d30](https://github.com/davidban77/sonda/commit/0e82d30661d30be623b8de97b89ac26699bd29b0))


### Documentation

* **1.9:** collapse user-facing docs to the four-verb CLI ([#359](https://github.com/davidban77/sonda/issues/359)) ([7893286](https://github.com/davidban77/sonda/commit/7893286977890502fc14a4ba414efa981f224206))
* **cli-reference:** rewrite for the 4-verb 1.9 CLI surface (1.9e) ([#358](https://github.com/davidban77/sonda/issues/358)) ([8bb9e1d](https://github.com/davidban77/sonda/commit/8bb9e1df37e8ea1aef74bbe069bcb47b46e4f0aa))


### Miscellaneous

* **1.9:** final-gate polish — docs sweep, smoke tests, validator rewrite ([#360](https://github.com/davidban77/sonda/issues/360)) ([9698bff](https://github.com/davidban77/sonda/commit/9698bfff5491309325fbbdbe40e1fcc58c87bf01))


### Refactoring

* **analysis:** move pattern detector from sonda CLI to sonda-core::analysis ([#354](https://github.com/davidban77/sonda/issues/354)) ([659cf42](https://github.com/davidban77/sonda/commit/659cf42687882627e28ff997342d9422e9f4ba80))

## [1.8.0](https://github.com/davidban77/sonda/compare/v1.7.0...v1.8.0) (2026-05-15)


### Features

* **csv-replay:** derive emission rate from CSV timestamps + default_metric_name fallback ([#344](https://github.com/davidban77/sonda/issues/344)) ([bd70663](https://github.com/davidban77/sonda/commit/bd706639663a5e595bdd78bc6d9f9359ae32c5d0))


### Documentation

* remove stale "New in 1.2.0" tip from landing page ([#346](https://github.com/davidban77/sonda/issues/346)) ([438c789](https://github.com/davidban77/sonda/commit/438c789e04af2b33f646f2db0d1ee82520cd2351))

## [1.7.0](https://github.com/davidban77/sonda/compare/v1.6.4...v1.7.0) (2026-05-15)


### Features

* **sink:** distinguish buffered vs delivered writes for /stats ([#294](https://github.com/davidban77/sonda/issues/294)) ([#341](https://github.com/davidban77/sonda/issues/341)) ([70a28a3](https://github.com/davidban77/sonda/commit/70a28a31eb0afb93b3888e1046c74d27f9abf1b2))


### Bug Fixes

* **sink:** time-based flush for batching sinks ([#297](https://github.com/davidban77/sonda/issues/297)) ([#338](https://github.com/davidban77/sonda/issues/338)) ([2f48b3a](https://github.com/davidban77/sonda/commit/2f48b3ae961ad4beac97cdab2d6e5cd2414ea372))


### Documentation

* visualize wedged-sink failure mode in /stats and /scenarios pages ([#343](https://github.com/davidban77/sonda/issues/343)) ([1b44895](https://github.com/davidban77/sonda/commit/1b448952fe8230cbfb6f887926c47a16b9f398d9))

## [1.6.4](https://github.com/davidban77/sonda/compare/v1.6.3...v1.6.4) (2026-05-07)


### Bug Fixes

* **core:** bump close-emit timestamp past most recent active emission ([#334](https://github.com/davidban77/sonda/issues/334)) ([591ceaa](https://github.com/davidban77/sonda/commit/591ceaa0e0fcc945c1d61b85cc509b6898f64288))

## [1.6.3](https://github.com/davidban77/sonda/compare/v1.6.2...v1.6.3) (2026-05-07)


### Documentation

* **scenarios:** recommend snap_to for remote_write integrations ([#331](https://github.com/davidban77/sonda/issues/331)) ([3b50b30](https://github.com/davidban77/sonda/commit/3b50b30ac7cc381340dde83ba2e7058a2776d603))

## [1.6.2](https://github.com/davidban77/sonda/compare/v1.6.1...v1.6.2) (2026-05-06)


### Bug Fixes

* **core:** structural refactor of gated_loop to make close-emit unmissable ([#327](https://github.com/davidban77/sonda/issues/327)) ([4a7910b](https://github.com/davidban77/sonda/commit/4a7910b4db2c6f176d58a785ba670901db1dccec))

## [1.6.1](https://github.com/davidban77/sonda/compare/v1.6.0...v1.6.1) (2026-05-05)


### Bug Fixes

* **core:** flush stale markers on Running exit via duration or shutdown ([#323](https://github.com/davidban77/sonda/issues/323)) ([282cd48](https://github.com/davidban77/sonda/commit/282cd48605dc8110872824b033fb365a6c53d2f4))

## [1.6.0](https://github.com/davidban77/sonda/compare/v1.5.0...v1.6.0) (2026-05-05)


### Features

* continuous coupling hardening — `while:` v1.6 ([#319](https://github.com/davidban77/sonda/issues/319)) ([191cf49](https://github.com/davidban77/sonda/commit/191cf4921b14eb56ef54b51286b7e2ab5312f88e))

## [1.5.0](https://github.com/davidban77/sonda/compare/v1.4.0...v1.5.0) (2026-05-04)


### Features

* continuous coupling — `while:` lifecycle gating ([#295](https://github.com/davidban77/sonda/issues/295)) ([#313](https://github.com/davidban77/sonda/issues/313)) ([ec70e53](https://github.com/davidban77/sonda/commit/ec70e538581d64c3bf5d0d663ccab372a7af3e1e))

## [1.4.0](https://github.com/davidban77/sonda/compare/v1.3.0...v1.4.0) (2026-05-02)


### Features

* sink-error policy and runner self-observability ([#293](https://github.com/davidban77/sonda/issues/293)) ([8993083](https://github.com/davidban77/sonda/commit/8993083d5c41d6466de88bf3f45640e8dfc7e722))


### Documentation

* **scenarios:** clarify v2 cascade duration semantics ([#291](https://github.com/davidban77/sonda/issues/291)) ([9b60b2d](https://github.com/davidban77/sonda/commit/9b60b2d1a007f9c97b55b3c6af4c129f353d8c09))


### Miscellaneous

* speed up tests with nextest + bound target/ size ([#298](https://github.com/davidban77/sonda/issues/298)) ([b7de2bd](https://github.com/davidban77/sonda/commit/b7de2bd40bd7338a897f9977be9d7b6a4180751b))

## [1.3.0](https://github.com/davidban77/sonda/compare/v1.2.2...v1.3.0) (2026-04-29)


### Features

* **server:** add POST /events for synchronous single-event emission ([#289](https://github.com/davidban77/sonda/issues/289)) ([b3d9880](https://github.com/davidban77/sonda/commit/b3d9880bb1b81987f54f8928eff11948ec4ee242))


### Bug Fixes

* **ci:** pin Release workflow toolchain to 1.95.0 so cross targets find core ([#287](https://github.com/davidban77/sonda/issues/287)) ([deddaad](https://github.com/davidban77/sonda/commit/deddaadfbe2c80e3f3bdf22f5770c13c8c1f0ae9))

## [1.2.2](https://github.com/davidban77/sonda/compare/v1.2.1...v1.2.2) (2026-04-27)


### Bug Fixes

* **server:** resolve packs from SONDA_PACK_PATH + scrape endpoint returns 200, not 204 ([#286](https://github.com/davidban77/sonda/issues/286)) ([e15deba](https://github.com/davidban77/sonda/commit/e15deba093651e829255d70a5ccfd85fa4b954ff))
* **sinks:** lower default batch_size for remote_write/loki/otlp_grpc 100 → 5 ([#285](https://github.com/davidban77/sonda/issues/285)) ([70b6048](https://github.com/davidban77/sonda/commit/70b60489d451864b9689a3f60c4be6e3fcb74b41))


### Miscellaneous

* **deps:** clear cargo audit warnings (5 → 1, residual documented) ([#283](https://github.com/davidban77/sonda/issues/283)) ([a1a9bbd](https://github.com/davidban77/sonda/commit/a1a9bbd32ae10e40f6f7698452445b72c530c4ac))

## [1.2.1](https://github.com/davidban77/sonda/compare/v1.2.0...v1.2.1) (2026-04-27)


### Bug Fixes

* **examples:** bump otel-collector-contrib to 0.150.1 + migrate loki exporter ([#282](https://github.com/davidban77/sonda/issues/282)) ([9828291](https://github.com/davidban77/sonda/commit/9828291dfb4d783bf7ba19a5a6e98f3e47e93da1))


### CI/CD

* bump grafana/loki from 3.5.5 to 3.7.1 in /examples ([#270](https://github.com/davidban77/sonda/issues/270)) ([cee3978](https://github.com/davidban77/sonda/commit/cee3978ca7176096c30c41e5dfbd8830d76d5509))
* bump mendhak/http-https-echo from 35 to 40 in /examples ([#269](https://github.com/davidban77/sonda/issues/269)) ([183fb10](https://github.com/davidban77/sonda/commit/183fb10c91daed4227b6de800f76e9d4569e6540))
* bump prom/alertmanager from v0.28.1 to v0.32.0 in /examples ([#275](https://github.com/davidban77/sonda/issues/275)) ([0ca0403](https://github.com/davidban77/sonda/commit/0ca04035abf5a77d0e54ddb67072fe3d00db8dbb))
* bump softprops/action-gh-release from 2 to 3 ([#200](https://github.com/davidban77/sonda/issues/200)) ([816d2a2](https://github.com/davidban77/sonda/commit/816d2a26d7631ba2e26d50011dffccbf65e96e3d))
* bump victoriametrics/vmalert from v1.108.1 to v1.140.0 in /examples ([#274](https://github.com/davidban77/sonda/issues/274)) ([fe5abc2](https://github.com/davidban77/sonda/commit/fe5abc264153dde6aaf8a666bba475637ea0ceb4))

## [1.2.0](https://github.com/davidban77/sonda/compare/v1.1.0...v1.2.0) (2026-04-26)


### Features

* env-var interpolation in v2 scenario YAML loader (closes [#223](https://github.com/davidban77/sonda/issues/223)) ([17b72a3](https://github.com/davidban77/sonda/commit/17b72a3f6864a52d84672004bbdeec7bd7bde54d))


### Bug Fixes

* docker entrypoint dispatch + http_push 4 KiB default (closes [#223](https://github.com/davidban77/sonda/issues/223)) ([8b5d4fa](https://github.com/davidban77/sonda/commit/8b5d4fa8fe58bdf98cc961c33acb3d7806d6b6d4))
* docker entrypoint dispatch + lower http_push batch_size to 4 KiB ([25b947a](https://github.com/davidban77/sonda/commit/25b947a73b017f86448d21dd5fbc8fea2b2140d0))
* **sonda-server:** bind port 0 + stdout announce eliminates test port race ([0675a09](https://github.com/davidban77/sonda/commit/0675a0973ef169157e73be1c6d0de8be2c496d80))
* **sonda-server:** bind port 0 + stdout announce eliminates test port race ([028242a](https://github.com/davidban77/sonda/commit/028242a17b4ab32e47f36fa331d1cc1bfeb50715))


### Documentation

* convert landing pages to Material grid cards + start-here callouts ([ad53d44](https://github.com/davidban77/sonda/commit/ad53d44ac0725fc3b87f7154331ce3e5f0c6dfb3))
* glossary tooltips, scroll-aware TOC, one tabbed snippet ([7acf772](https://github.com/davidban77/sonda/commit/7acf77255d708ba7d1e98e99dccfab2848b0d0a1))
* rework landing page in FastAPI-shape, drop story-led intro ([8dacd6d](https://github.com/davidban77/sonda/commit/8dacd6dfb8a8bfe4fb6a6aaaf821620be900ce52))
* story-lead the landing page (extends PR [#278](https://github.com/davidban77/sonda/issues/278) pattern) ([690b072](https://github.com/davidban77/sonda/commit/690b0722d878103739e50ba121a4ac09cd9a1908))
* story-led guide intros, section landing pages, generator chooser table ([82e461f](https://github.com/davidban77/sonda/commit/82e461f3d31bb88589bcee20110e4628e4ac085e))
* tone-match env-var interpolation prose + sweep cross-links ([6366694](https://github.com/davidban77/sonda/commit/636669460a9b684a453d90303db1e2cbec127045))


### Miscellaneous

* drop hand-written CHANGELOG entry — release-please owns the file ([9881eb4](https://github.com/davidban77/sonda/commit/9881eb4c6ed44a4725c4c50cdb65ea730d68e02f))
* trim verbose comments + docstrings on port-zero harness ([291a1a7](https://github.com/davidban77/sonda/commit/291a1a77dd8a1b23ea19d74fdef00f08586de445))
* trim verbose comments + tighten getting-started prose ([2344836](https://github.com/davidban77/sonda/commit/23448365498c9f17dd3adbe33dbfe3d154c1340d))

## [1.1.0](https://github.com/davidban77/sonda/compare/v1.0.1...v1.1.0) (2026-04-25)


### Features

* **sonda-server:** warn on POST when a sink URL targets loopback ([3329f92](https://github.com/davidban77/sonda/commit/3329f9260efea2d4dcb4202ab1018159e36a1cde))
* **sonda-server:** warn on POST when a sink URL targets loopback (audit FU-2) ([3da214e](https://github.com/davidban77/sonda/commit/3da214e153241213d7cc16678ca1644dbbd04ebc))


### Bug Fixes

* **examples:** migrate alerting-scenario to v2 + recurse example sweep ([a3bc86c](https://github.com/davidban77/sonda/commit/a3bc86cf96a96227a874118dbaacfc76ace2e91b))
* **sink:** replace hand-rolled tmp_path with tempfile::TempDir ([11f039d](https://github.com/davidban77/sonda/commit/11f039db862773a2cc22376230f05955f6989f7a))
* **sonda-server:** wire clap version flag + document in CLI reference ([68adf75](https://github.com/davidban77/sonda/commit/68adf75eb039f0e3a1946f46403790279378dc38))


### Documentation

* **alert-testing:** split monolithic page into 6 progressive sub-pages ([8b75383](https://github.com/davidban77/sonda/commit/8b7538361294fef4b333ee97c2750dead28792f8))
* **cli:** add sonda-server section + fix sonda-server.md cross-ref ([800e607](https://github.com/davidban77/sonda/commit/800e607ff2bbdab5afa0ddfc4b68ab750b19b4ed))
* **cli:** add sonda-server section + fix sonda-server.md cross-ref (audit P1-1) ([dba0a13](https://github.com/davidban77/sonda/commit/dba0a130373b3c58d06b25cc8a26971a87364936))
* **e2e:** close 3 of 4 matrix gaps + trim curl/jq dup + repoint 2 examples ([621a3e6](https://github.com/davidban77/sonda/commit/621a3e6e65734d7c6bc0ca4f8663765482bb93fc))
* **e2e:** close 3 of 4 matrix gaps + trim curl/jq dup + repoint 2 examples (issue [#245](https://github.com/davidban77/sonda/issues/245) + audit FU-6) ([00c63ed](https://github.com/davidban77/sonda/commit/00c63eddd1e1b6a7e671f6a8bd0e83a304cf281d))
* **e2e:** rewrite guide user-facing + lift contributor content + drop CI skip ([a28db0d](https://github.com/davidban77/sonda/commit/a28db0d8adfd0cb338b6cb645c553abba443c176))
* **guides:** trim pipeline-validation E2E section + capacity-planning intro ([fbbb5a8](https://github.com/davidban77/sonda/commit/fbbb5a8b268bd3433e9b2114283af64406d50ea3))
* **site:** add endpoints & networking page + fix Compose stack honesty ([568f063](https://github.com/davidban77/sonda/commit/568f0633b6e77717eabb227c0f9c5db8d57aabe4))
* **site:** announce v1.0.1 crates.io release + fix k8s servicemonitor anchor ([319b796](https://github.com/davidban77/sonda/commit/319b7969c772f1217e7af0801442390f4816868e))
* **site:** fill 16 missing examples + Dynamic Labels guide + polish ([c8d5ca7](https://github.com/davidban77/sonda/commit/c8d5ca7e601abf5983aa8afc4cbe3b90e08629ca))
* **site:** rename scenario-file → scenario-fields + regroup Guides nav ([817b3ab](https://github.com/davidban77/sonda/commit/817b3abe10fbe3cd3af665de75e972c46eab5cae))
* **tutorial:** split monolithic tutorial into 8 progressive pages ([a718e5e](https://github.com/davidban77/sonda/commit/a718e5e5f95c9816ce37410c029cdb937302b9fe))
* **tutorial:** split monolithic tutorial into 8 progressive pages (audit P2-1) ([3946ff3](https://github.com/davidban77/sonda/commit/3946ff36cfc6f1713a5629d8fce80c9e1deb011b))


### Miscellaneous

* **examples:** trim otlp-metrics duration + pin grafana/loki to 3.5.5 ([ea77af4](https://github.com/davidban77/sonda/commit/ea77af49692fab481dd600c56af3b6c3f5ffa7e0))
* **release-please:** remove release-as pin after v1.0.1 ([0196dd7](https://github.com/davidban77/sonda/commit/0196dd7d66d1b27f76c2eb60a77a3f104a127da8))
* **scripts:** trim narrative comments in docs-drift catcher ([60aa8c5](https://github.com/davidban77/sonda/commit/60aa8c5fc17e72152cd0c873ef62fe1f25452d95))
* **sonda-server:** trim verbose docstrings on FU-2 helpers ([bc55735](https://github.com/davidban77/sonda/commit/bc55735292f29add31b66202960d7751cb97925a))
* **tooling:** pin rust toolchain to 1.95.0 via rust-toolchain.toml ([5454311](https://github.com/davidban77/sonda/commit/54543111253c930b1ed37291a58089529910fb1e))


### CI/CD

* add docs-drift catcher for sonda commands in user-facing docs ([88fdd4f](https://github.com/davidban77/sonda/commit/88fdd4fdee347264b2c2439e1600522f9d91eaee))
* bump apache/kafka from 4.1.2 to 4.2.0 in /examples ([a17a4e1](https://github.com/davidban77/sonda/commit/a17a4e1bbb81c1b4b3faa7442b23cd8f5334176f))
* bump apache/kafka from 4.1.2 to 4.2.0 in /examples ([a0ecf6f](https://github.com/davidban77/sonda/commit/a0ecf6f7a315ab2a7c535088833f0a74bc61e3f8))
* bump googleapis/release-please-action from 4 to 5 ([f892d57](https://github.com/davidban77/sonda/commit/f892d573ef180f9c22eb096415b1e9a7c7876c0d))
* bump googleapis/release-please-action from 4 to 5 ([e2eef51](https://github.com/davidban77/sonda/commit/e2eef5153c0c7c9873e25c178dfdf934aeacb775))
* bump grafana/grafana from 11.5.2 to 13.0.1 in /examples ([be01ff7](https://github.com/davidban77/sonda/commit/be01ff7246e438d3f100354cdb897f4670f60941))
* bump grafana/grafana from 11.5.2 to 13.0.1 in /examples ([e7ff1e1](https://github.com/davidban77/sonda/commit/e7ff1e10efcad83d78c00d1cac3c1b8485f399bb))
* bump prom/prometheus from v2.55.1 to v3.11.2 in /examples ([cff0e17](https://github.com/davidban77/sonda/commit/cff0e17d07f3208c0cc05515eefc3f37cb599c9e))
* bump prom/prometheus from v2.55.1 to v3.11.2 in /examples ([ed6ea4c](https://github.com/davidban77/sonda/commit/ed6ea4ce4251dde8e54240541a929921542da220))
* bump victoriametrics/victoria-metrics from v1.108.1 to v1.140.0 in /examples ([099134d](https://github.com/davidban77/sonda/commit/099134dcaa2da4c80593c18bd68132a31755c3d0))
* bump victoriametrics/victoria-metrics in /examples ([fbd8fa3](https://github.com/davidban77/sonda/commit/fbd8fa38ac69ccd08baefd10756b603fdeb869f9))
* bump victoriametrics/vmagent from v1.108.1 to v1.140.0 in /examples ([2b4ac24](https://github.com/davidban77/sonda/commit/2b4ac24cfaa692457f628aed4de50f9efbd907ba))
* bump victoriametrics/vmagent from v1.108.1 to v1.140.0 in /examples ([4f9580f](https://github.com/davidban77/sonda/commit/4f9580f2a7cce6f18b73e24a0c1bb45b18592ad0))
* **live-infra-uat:** validate e2e matrix against live container backends ([6a5eb54](https://github.com/davidban77/sonda/commit/6a5eb54b3dcdef841ca06dc395159fee5351688c))
* **live-infra:** wire OTel collector health_check extension end-to-end ([c13e25d](https://github.com/davidban77/sonda/commit/c13e25d50af711cb8516d8d5b390cc745320d6a6))


### Refactoring

* **scenario-loader:** consolidate v2 compile dispatch + promote discriminant ([ab899d6](https://github.com/davidban77/sonda/commit/ab899d6973628f241f659e78eb5418fd6331d133))

## [1.0.1](https://github.com/davidban77/sonda/compare/v1.0.0...v1.0.1) (2026-04-22)

Maintenance release preparing sonda for its first publish to [crates.io](https://crates.io/crates/sonda-core). No user-visible behavior change vs. v1.0.0 — CLI and server are functionally identical.

### Library integrators — note on `#[non_exhaustive]`

`sonda-core` marks its public error enums, config enums, and `ScenarioStats` as `#[non_exhaustive]` so future variants/fields can be added without a major bump. If you embed `sonda-core` as a library:

- `match` on `SondaError`, `ConfigError`, `GeneratorError`, `EncoderError`, `RuntimeError`, `CompileError`, the five compile-phase error enums (`ParseError`, `NormalizeError`, `ExpandError`, `CompileAfterError`, `PrepareError`), `GeneratorConfig`, `EncoderConfig`, `SinkConfig`, `DistributionConfig`, and `ScenarioEntry` now requires a wildcard `_ =>` arm.
- Struct literals for `ScenarioStats` must use `..Default::default()` (or `ScenarioStats::default()`) for forward compatibility.

Sonda was not previously published to crates.io, so no released consumer breaks.

### Bug Fixes

* repair `--all-features` gate failures + add CI coverage ([78f1501](https://github.com/davidban77/sonda/commit/78f1501bc5d21997fe457f2a2583bbffb5e2c413))

### Miscellaneous

* **api:** mark public enums `#[non_exhaustive]` before crates.io publish ([88faa0c](https://github.com/davidban77/sonda/commit/88faa0c2fd5c3ed34fba19ea5a94a62fe2075093))
* **deps:** bump rustls-webpki to 0.103.13 for [RUSTSEC-2026-0104](https://rustsec.org/advisories/RUSTSEC-2026-0104) ([026ac71](https://github.com/davidban77/sonda/commit/026ac716eb3fc04c365729b013a51fa5024def02))

## [1.0.0](https://github.com/davidban77/sonda/compare/v0.15.0...v1.0.0) (2026-04-21)

First `v1.0.0` milestone release. Ships the unified v2 scenario format across the full stack (CLI, library, HTTP server). v1 YAML is fully retired — all built-in scenarios, examples, and input paths now speak v2.

**Migration guide:** [`docs/configuration/v2-scenarios.md`](https://github.com/davidban77/sonda/blob/main/docs/site/docs/configuration/v2-scenarios.md)


### ⚠ BREAKING CHANGES

#### CLI — v1 YAML files rejected ([#216](https://github.com/davidban77/sonda/pull/216))

Every CLI path that accepted a YAML scenario file (`sonda run --scenario`, `sonda metrics --scenario`, `sonda logs --scenario`, `sonda histogram --scenario`, `sonda summary --scenario`, `sonda catalog run`, `sonda scenarios run`) now **requires v2 YAML**. v1 files surface a clear migration error pointing at the v2 guide.

**Before (v1):**

```yaml
name: cpu_usage
rate: 1
duration: 30s
generator: { type: sine, amplitude: 10, period: 60s }
encoder: { type: prometheus_text }
sink: { type: stdout }
```

**After (v2):**

```yaml
version: 2
defaults:
  rate: 1
  duration: 30s
  encoder: { type: prometheus_text }
  sink: { type: stdout }
scenarios:
  - signal_type: metrics
    name: cpu_usage
    generator: { type: sine, amplitude: 10, period: 60s }
```

#### CLI — `sonda story` subcommand removed ([#215](https://github.com/davidban77/sonda/pull/215))

Multi-signal temporal scenarios are now expressed directly as v2 scenarios with entry-level `after:` clauses — no separate `story` subcommand. Replace `sonda story --file stories/link-failover.yaml` with `sonda run --scenario scenarios/link-failover.yaml` or `sonda catalog run link-failover`. The canonical causal-chain example (`scenarios/link-failover.yaml`) ships as a built-in v2 scenario demonstrating the flap → saturation → degradation pattern.

#### Library — `MultiScenarioConfig` removed from public API ([#216](https://github.com/davidban77/sonda/pull/216))

`sonda_core::config::MultiScenarioConfig` is deleted. Library integrators calling `run_multi` must update:

```rust
// Before:
run_multi(MultiScenarioConfig { scenarios: v }, shutdown)

// After:
run_multi(v, shutdown)  // pass Vec<ScenarioEntry> directly
```

#### Server — `POST /scenarios` accepts v2 YAML/JSON only ([#216](https://github.com/davidban77/sonda/pull/216))

`sonda-server` endpoints now accept only v2-shape bodies (`version: 2` at root + `scenarios:` list). v1 bodies return **HTTP 400** with a JSON error including a migration hint pointing at the v2 guide.


### ✨ What's new in v2

- **Unified scenario format** — every built-in (`scenarios/*.yaml`), example (`examples/*.yaml`), and input path speaks v2. One shape for single-signal, multi-signal, and pack-backed scenarios.
- **Causal chains via `after:` clauses** — express "B starts after A crosses threshold N" declaratively; signal offsets compile deterministically from generator timing math.
- **Catalog metadata** — v2 scenarios carry `scenario_name` / `category` / `description` at root for `sonda scenarios list` / `sonda catalog list`.
- **Multi-signal detection** — catalog probe reports `signal: multi` automatically for v2 files with multiple entries.
- **Pack integration** — v2 scenarios reference packs as first-class entries with `pack:` + `overrides:`.
- **CLI unification** — `sonda run --scenario <file>` handles any signal type (metrics / logs / histogram / summary / multi) transparently.
- **Server v2 acceptance** — `POST /scenarios` accepts v2 YAML or JSON bodies, atomically launches all scenario entries.


### Features

* **cli:** v2 CLI unification — sonda run dispatch, catalog, init, dry-run (v2 PR 7) ([#206](https://github.com/davidban77/sonda/issues/206)) ([957009f](https://github.com/davidban77/sonda/commit/957009fb6c1a95b3eea9934487b2eec0568bf5d7))
* **core+cli:** v2 ScenarioFile metadata + steady-state migration (v2 PR 8a.1) ([#208](https://github.com/davidban77/sonda/issues/208)) ([c1c3ec6](https://github.com/davidban77/sonda/commit/c1c3ec6a2e3b49f33cd4ac20ce5defb51cadcc25))
* **core:** after-clause compilation and dependency graph (v2 PR 5) ([#203](https://github.com/davidban77/sonda/issues/203)) ([8d540ef](https://github.com/davidban77/sonda/commit/8d540eff1b6bd2712d708fc7a9bd6e8ae58d3d0a))
* **core:** compile snapshot harness and test foundation ([5062e50](https://github.com/davidban77/sonda/commit/5062e5006ddfea468eae7abb9845e8d5063b6f49))
* **core:** defaults resolution and normalization for v2 compiler ([#199](https://github.com/davidban77/sonda/issues/199)) ([c39f4fe](https://github.com/davidban77/sonda/commit/c39f4feed1a56488c53526f53077fd7769651421))
* **core:** pack expansion inside scenarios: (v2 PR 4) ([#202](https://github.com/davidban77/sonda/issues/202)) ([6a22dcc](https://github.com/davidban77/sonda/commit/6a22dccefb85ab07a409883d7c0f39d3ca2fb101))
* **core:** runtime wiring + parity tests (v2 PR 6) ([#205](https://github.com/davidban77/sonda/issues/205)) ([5953a5c](https://github.com/davidban77/sonda/commit/5953a5ca6db874a5a9baa92d86d5f7a78f42a6f6))
* **core:** v2 AST, parser, and version dispatch (v2 PR 2) ([#198](https://github.com/davidban77/sonda/issues/198)) ([383bd0c](https://github.com/davidban77/sonda/commit/383bd0cdb409ea97a4ff8e15e34dc1732d7c79ab))
* **scenarios:** v2 link-failover migration + network-link-failure dedup + docs (v2 PR 8a.3) ([#214](https://github.com/davidban77/sonda/issues/214)) ([f1d9b00](https://github.com/davidban77/sonda/commit/f1d9b0075c1a18617284b22d628ec2c4acf15782))


### Bug Fixes

* **cli:** detect v2 multi-entry scenarios in catalog probe (v2 PR 8a.2b.0) ([#211](https://github.com/davidban77/sonda/issues/211)) ([398430e](https://github.com/davidban77/sonda/commit/398430eb2f96b216cedab028c0ab50528026213a))
* **cli:** infer signal_type from first entry for v2 scenarios (v2 PR 8a.2a) ([#210](https://github.com/davidban77/sonda/issues/210)) ([00cbf1a](https://github.com/davidban77/sonda/commit/00cbf1a20b23fd640ce1bfe818f619ab444614d7))
* **core:** address reviewer findings on snapshot harness ([9d9ce42](https://github.com/davidban77/sonda/commit/9d9ce42441d92c2c077352c27e11ce2bb8614f21))


### Miscellaneous

* **cli:** retire v1 story subcommand + shipped story + parity bridge (v2 PR 9a) ([#215](https://github.com/davidban77/sonda/issues/215)) ([3706d67](https://github.com/davidban77/sonda/commit/3706d67cce673665beed6e48d89d3cd206dce514))
* **examples:** migrate 61 example scenario YAMLs to v2 format ([#218](https://github.com/davidban77/sonda/issues/218)) ([06f5122](https://github.com/davidban77/sonda/commit/06f5122a9230260bbae4c6e5da1ab07da7fc0f65))
* **refactor:** add parity bridge tests to validation matrix (sections 16-17) ([797704b](https://github.com/davidban77/sonda/commit/797704bfc433824e579f5f22b3c7de78858f120a))
* **refactor:** add progress tracker, validation matrix, and gitignore for v2 docs ([0af830f](https://github.com/davidban77/sonda/commit/0af830f33c04127854d729dda9a9f01d07786338))
* **scenarios:** batch migrate 10 built-in scenarios to v2 + fix catalog dispatch (v2 PR 8a.2b) ([#212](https://github.com/davidban77/sonda/issues/212)) ([44c8514](https://github.com/davidban77/sonda/commit/44c85149e9940d6765e29862f9a6985a1782465f))
* **test:** adopt insta + rstest, consolidate v2 test infra ([#204](https://github.com/davidban77/sonda/issues/204)) ([e6c1cc4](https://github.com/davidban77/sonda/commit/e6c1cc47da19a8494c05759efd8dffdf30fafc81))
* **test:** drop v2_ prefix on test files + prune + collapse pack parity (v2 PR 9c) ([#217](https://github.com/davidban77/sonda/issues/217)) ([b43a0e8](https://github.com/davidban77/sonda/commit/b43a0e8b8f04eeaba271809dfe70a1e63460a7b8))
* **test:** parametrize encoder_sink_matrix + redact null snapshots ([#207](https://github.com/davidban77/sonda/issues/207)) ([aa4fe55](https://github.com/davidban77/sonda/commit/aa4fe5572a8b26639625d38c6775393b520f8564))


### Refactoring

* **core+cli+server:** full v1 YAML retirement + server v2 acceptance (v2 PR 9b) ([#216](https://github.com/davidban77/sonda/issues/216)) ([02693b7](https://github.com/davidban77/sonda/commit/02693b724d515680c4e80fd5c8dbdb41d3a210da))
* unified v2 scenario model (integration branch) ([864d6a5](https://github.com/davidban77/sonda/commit/864d6a5b6ecd25adf806d29ca6cdf85734a36601))

## [0.15.0](https://github.com/davidban77/sonda/compare/v0.14.0...v0.15.0) (2026-04-10)


### Features

* **story:** composable multi-signal story compilation layer ([#195](https://github.com/davidban77/sonda/issues/195)) ([61a5fed](https://github.com/davidban77/sonda/commit/61a5feddf1e9a5473950ddbca4a1fb5944e49847))

## [0.14.0](https://github.com/davidban77/sonda/compare/v0.13.0...v0.14.0) (2026-04-09)


### Features

* **init:** histogram and summary signal types ([#194](https://github.com/davidban77/sonda/issues/194)) ([4f49a64](https://github.com/davidban77/sonda/commit/4f49a64dc2b5d8e6c193983de3441cebd36c7a42))
* **init:** non-interactive mode and --from prefill ([#192](https://github.com/davidban77/sonda/issues/192)) ([0693564](https://github.com/davidban77/sonda/commit/0693564d7d32523b8dc05d3b8088d35ce1775ce5))

## [0.13.0](https://github.com/davidban77/sonda/compare/v0.12.0...v0.13.0) (2026-04-09)


### Features

* `sonda init` — guided scenario scaffolding ([#190](https://github.com/davidban77/sonda/issues/190)) ([cef1284](https://github.com/davidban77/sonda/commit/cef128459633b4156c0fec0c7a7c1cb5652014a6))
* **init:** UX polish — pack filtering, advanced sinks, immediate execution ([#191](https://github.com/davidban77/sonda/issues/191)) ([99f7dd3](https://github.com/davidban77/sonda/commit/99f7dd34844bfd0b73dd9cde7ef5d053c08fd48a))
* metric packs — domain-specific label and name bundles ([#185](https://github.com/davidban77/sonda/issues/185)) ([247c04a](https://github.com/davidban77/sonda/commit/247c04a0c07aaa7eee0b444392fee64d37da9373))
* operational vocabulary layer for generators ([#183](https://github.com/davidban77/sonda/issues/183)) ([b944fc6](https://github.com/davidban77/sonda/commit/b944fc6429f40ff6f2261cc23ccd64166b424303))
* sonda import — convert CSV to parameterized scenario ([#188](https://github.com/davidban77/sonda/issues/188)) ([05b218c](https://github.com/davidban77/sonda/commit/05b218c9aa30f12b402b19cbb4571903a0b94f9f))


### Refactoring

* externalize built-in scenarios from sonda-core binary ([#187](https://github.com/davidban77/sonda/issues/187)) ([83cf787](https://github.com/davidban77/sonda/commit/83cf787539ce16d1db26f836e65ce141237291db))

## [0.12.0](https://github.com/davidban77/sonda/compare/v0.11.0...v0.12.0) (2026-04-08)


### Features

* CLI UX polish — help grouping, hints, banners, list styling ([#174](https://github.com/davidban77/sonda/issues/174)) ([783f1ef](https://github.com/davidban77/sonda/commit/783f1eff53c6d51fa60929c827b896cfa07e41c6))
* live progress indicator during scenario execution ([#176](https://github.com/davidban77/sonda/issues/176)) ([f29bce2](https://github.com/davidban77/sonda/commit/f29bce2c737a4264cec24c0b69e5c8f26b1e2253))

## [0.11.0](https://github.com/davidban77/sonda/compare/v0.10.0...v0.11.0) (2026-04-07)


### Features

* add API key authentication for sonda-server ([#169](https://github.com/davidban77/sonda/issues/169)) ([c7f28c1](https://github.com/davidban77/sonda/commit/c7f28c1368ccb034c31a5428e33dce282b201702))
* beautify CLI output ([#172](https://github.com/davidban77/sonda/issues/172)) ([1d47c86](https://github.com/davidban77/sonda/commit/1d47c863f455e1f05932fda9a8a0bae009528c72))
* POST /scenarios accepts multi-scenario YAML ([#171](https://github.com/davidban77/sonda/issues/171)) ([2cf775a](https://github.com/davidban77/sonda/commit/2cf775a988d90af9d06e6bf9a30d5477fb95fee3))

## [0.10.0](https://github.com/davidban77/sonda/compare/v0.9.0...v0.10.0) (2026-04-07)


### Features

* pre-built scenario library for common patterns ([#167](https://github.com/davidban77/sonda/issues/167)) ([82a690f](https://github.com/davidban77/sonda/commit/82a690fd8215e7c876849d9dc40d6a3588068d19))

## [0.9.0](https://github.com/davidban77/sonda/compare/v0.8.0...v0.9.0) (2026-04-07)


### Features

* add TLS and SASL support for Kafka sink ([#165](https://github.com/davidban77/sonda/issues/165)) ([e2a785e](https://github.com/davidban77/sonda/commit/e2a785ee974ba8c7535e1698762c44fcb2ff1cef))


### Documentation

* add troubleshooting guide and performance baselines ([#166](https://github.com/davidban77/sonda/issues/166)) ([8b13c17](https://github.com/davidban77/sonda/commit/8b13c170fb4b8a7260d8032d08364013b0845bf5))
* trim README to lean overview, move detail to docs site ([#163](https://github.com/davidban77/sonda/issues/163)) ([a8a9204](https://github.com/davidban77/sonda/commit/a8a9204ce2b2b215f29b3c4c9d41cb2157d702c7))

## [0.8.0](https://github.com/davidban77/sonda/compare/v0.7.0...v0.8.0) (2026-04-06)


### Features

* add histogram and summary generators ([#149](https://github.com/davidban77/sonda/issues/149)) ([#160](https://github.com/davidban77/sonda/issues/160)) ([52c5e36](https://github.com/davidban77/sonda/commit/52c5e361adce564811ce202cefd614fbf1ae1e78))
* label-aware CSV replay for Grafana exports ([#162](https://github.com/davidban77/sonda/issues/162)) ([09218d1](https://github.com/davidban77/sonda/commit/09218d13909d8d1a3a22b853fc1a98db4cddb94e))

## [0.7.0](https://github.com/davidban77/sonda/compare/v0.6.0...v0.7.0) (2026-04-05)


### Features

* add retry/backoff for sink writes ([#159](https://github.com/davidban77/sonda/issues/159)) ([248bf73](https://github.com/davidban77/sonda/commit/248bf7366d9a39665552ad12447219bc6cdc1db2))
* harden Helm chart, add OTLP to Docker build, fix docs version drift ([#151](https://github.com/davidban77/sonda/issues/151), [#152](https://github.com/davidban77/sonda/issues/152), [#153](https://github.com/davidban77/sonda/issues/153)) ([#156](https://github.com/davidban77/sonda/issues/156)) ([f97ee53](https://github.com/davidban77/sonda/commit/f97ee530d22d5978e5439868224d584337216586))

## [0.6.0](https://github.com/davidban77/sonda/compare/v0.5.0...v0.6.0) (2026-04-05)


### Features

* add CLI flags for complex encoder/sink combos (OTLP, remote_write) ([#144](https://github.com/davidban77/sonda/issues/144)) ([d71a829](https://github.com/davidban77/sonda/commit/d71a82935e00fa8ea2c77c76f2a4c60f047e469a))
* add multi-column csv_replay support ([#147](https://github.com/davidban77/sonda/issues/147)) ([ea8c5f5](https://github.com/davidban77/sonda/commit/ea8c5f55637c0c5bde4ec65c68e6e7e328108631))
* label cardinality simulation (rotating hostnames, pod names) ([#146](https://github.com/davidban77/sonda/issues/146)) ([672b64e](https://github.com/davidban77/sonda/commit/672b64e90d79d51a26f7dd1279b443ede6f4bbf6))

## [0.5.0](https://github.com/davidban77/sonda/compare/v0.4.0...v0.5.0) (2026-04-04)


### Features

* add --value flag for constant generator ([#143](https://github.com/davidban77/sonda/issues/143)) ([9454759](https://github.com/davidban77/sonda/commit/945475944e7d9a0065f7ffd1843d01a07c9d00fa))
* add jitter option for realistic generator noise ([#134](https://github.com/davidban77/sonda/issues/134)) ([b479912](https://github.com/davidban77/sonda/commit/b479912a50162eb8d89355ce586fcf005649fc5f))
* add OTLP encoder and gRPC sink for OpenTelemetry ([#136](https://github.com/davidban77/sonda/issues/136)) ([89248df](https://github.com/davidban77/sonda/commit/89248dfbbd43776e2aef58eb328062e75042776b))
* add spike generator for anomaly simulation ([#133](https://github.com/davidban77/sonda/issues/133)) ([a8fff47](https://github.com/davidban77/sonda/commit/a8fff476be2bd8abece93a7f9cbc364f3ed0545d))
* add step generator for monotonic counters ([#131](https://github.com/davidban77/sonda/issues/131)) ([ddbd316](https://github.com/davidban77/sonda/commit/ddbd316f206d40ffd910bae3b5530391e32e0a65))


### Documentation

* add capacity planning with synthetic load guide ([#142](https://github.com/davidban77/sonda/issues/142)) ([a8ea617](https://github.com/davidban77/sonda/commit/a8ea61751c82b4d801734c3f812f6996cc58e241))
* add CI/CD alert rule validation guide ([#137](https://github.com/davidban77/sonda/issues/137)) ([95a2e1f](https://github.com/davidban77/sonda/commit/95a2e1ff3ae9429bff5398466bc5b3aae9598aa5))
* add network automation testing guide ([#96](https://github.com/davidban77/sonda/issues/96)) ([#141](https://github.com/davidban77/sonda/issues/141)) ([88478b6](https://github.com/davidban77/sonda/commit/88478b666574f04022f22aeba3a32dab88560be6))
* add network device telemetry guide ([#97](https://github.com/davidban77/sonda/issues/97)) ([#140](https://github.com/davidban77/sonda/issues/140)) ([479a5ae](https://github.com/davidban77/sonda/commit/479a5ae013e267930095880bbb6deb8a08138db1))
* add synthetic monitoring guide ([#99](https://github.com/davidban77/sonda/issues/99)) ([#138](https://github.com/davidban77/sonda/issues/138)) ([3751429](https://github.com/davidban77/sonda/commit/375142932a861dc03773714c3482444cccf33405))


### Miscellaneous

* add Kubernetes/k3d smoke testing to smoke agent ([#139](https://github.com/davidban77/sonda/issues/139)) ([4901475](https://github.com/davidban77/sonda/commit/49014750ac602ba09ac0faf08366c431a151ad8f))

## [0.4.0](https://github.com/davidban77/sonda/compare/v0.3.0...v0.4.0) (2026-04-03)


### Features

* add cardinality spikes for dynamic label injection ([#58](https://github.com/davidban77/sonda/issues/58)) ([9ecc9bb](https://github.com/davidban77/sonda/commit/9ecc9bb11b551379ecdac5f4fb20b6ce75a82a18))
* add decimal precision control to encoder configs ([#55](https://github.com/davidban77/sonda/issues/55)) ([d2fff52](https://github.com/davidban77/sonda/commit/d2fff522c438dc40795e2262258ce6efbcb076ae))
* **cli:** add --dry-run, --verbose flags and run aggregate summary ([#120](https://github.com/davidban77/sonda/issues/120)) ([ffde777](https://github.com/davidban77/sonda/commit/ffde777b52be2c94648b370a548166551b65b029))
* feature-gate HTTP sinks behind http Cargo feature (9C.1) ([#115](https://github.com/davidban77/sonda/issues/115)) ([d3d78c6](https://github.com/davidban77/sonda/commit/d3d78c6bc2ec2ccbf6980cafaa966ccdf54cd493))
* **slice-9A.2:** replace .expect() with .map_err() on lock acquisitions ([#106](https://github.com/davidban77/sonda/issues/106)) ([d782e50](https://github.com/davidban77/sonda/commit/d782e509b99281791c4e97c0818fa35b90f43638))
* **slice-9A.3:** disambiguate SondaError::Sink from generator I/O errors ([#108](https://github.com/davidban77/sonda/issues/108)) ([db945c6](https://github.com/davidban77/sonda/commit/db945c64b5131345ebfa6ce2d021e9dfbdbf1f6d))
* **slice-9A.4:** fix tick-as-usize truncation on 32-bit platforms ([#109](https://github.com/davidban77/sonda/issues/109)) ([446b0be](https://github.com/davidban77/sonda/commit/446b0be855c8ed813332c5678956785fb0c1c5b0))
* **slice-9B.1:** eliminate per-tick name.clone() and labels.clone() in metric runner ([#110](https://github.com/davidban77/sonda/issues/110)) ([46fd247](https://github.com/davidban77/sonda/commit/46fd24766dac851ee7b2e407423a9012cba241e5))
* **slice-9B.2:** add ValidatedMetricName newtype for compile-time metric name safety ([#111](https://github.com/davidban77/sonda/issues/111)) ([f81e59f](https://github.com/davidban77/sonda/commit/f81e59f04ae0de2b0dc2dd21d71d3c17671c6c95))
* **slice-9B.3:** eliminate per-event String allocation in timestamp formatting ([#112](https://github.com/davidban77/sonda/issues/112)) ([b48fcce](https://github.com/davidban77/sonda/commit/b48fccef04687a75496a7f367447d1eee7334602))
* **slice-9B.4:** eliminate intermediate BTreeMap allocation in JSON encoder ([#113](https://github.com/davidban77/sonda/issues/113)) ([f3dbaac](https://github.com/davidban77/sonda/commit/f3dbaac3c829510997bde3e96ce5be0b32f7058f))
* **slice-9B.5:** single-pass log template placeholder resolution ([#114](https://github.com/davidban77/sonda/issues/114)) ([850559e](https://github.com/davidban77/sonda/commit/850559e32f17ae510965c7ea2e4abf774362b3e5))
* **slice-9C.2:** feature-gate serde_yaml behind config Cargo feature ([#116](https://github.com/davidban77/sonda/issues/116)) ([9dca327](https://github.com/davidban77/sonda/commit/9dca32750677ffbce80219f7bb208b347cb2c7e1))
* **slice-9C.3:** migrate from serde_yaml to serde_yaml_ng ([#117](https://github.com/davidban77/sonda/issues/117)) ([7436a75](https://github.com/davidban77/sonda/commit/7436a75c6aed5489f194de9c8497cf93040b6f8d))
* **slice-9D.2:** unify ScenarioConfig and LogScenarioConfig via BaseScheduleConfig ([#127](https://github.com/davidban77/sonda/issues/127)) ([c5437d0](https://github.com/davidban77/sonda/commit/c5437d04e6f5888fb0e8d6f03588efe5444699b9))
* structured error sub-enums for SondaError (9C.4) ([#121](https://github.com/davidban77/sonda/issues/121)) ([1679279](https://github.com/davidban77/sonda/commit/1679279714f5279a380e8d00e991438ce3a326e2))


### Bug Fixes

* **9A.2-hardening:** handle poisoned locks, add force_stopped/panic tests ([#107](https://github.com/davidban77/sonda/issues/107)) ([ecee344](https://github.com/davidban77/sonda/commit/ecee3447518d9987527ab40554c4b0879df83dbe))
* add healthchecks to vmalert/alertmanager and note first-run build time ([#126](https://github.com/davidban77/sonda/issues/126)) ([712d82c](https://github.com/davidban77/sonda/commit/712d82cf764f09395a84699d042d478b3b4eb8d3))
* delete_scenario memory leak — remove handle from map (9A.1) ([#105](https://github.com/davidban77/sonda/issues/105)) ([73f16a2](https://github.com/davidban77/sonda/commit/73f16a2ae20327b7492f1abcf589b3d658635860))
* **docker:** replace removed bitnami/kafka:3.9 with apache/kafka:4.1.2 ([#62](https://github.com/davidban77/sonda/issues/62)) ([ef115f2](https://github.com/davidban77/sonda/commit/ef115f2c09b31192ffc03d168be79cb8c8c59efa))
* **docs:** make Loki/Kafka walkthrough sections self-contained ([#54](https://github.com/davidban77/sonda/issues/54)) ([75335a9](https://github.com/davidban77/sonda/commit/75335a9fa61adb14d7b8dae4cb7454c87c7cff69))
* **docs:** show full incident pattern in CSV replay walkthrough ([#57](https://github.com/davidban77/sonda/issues/57)) ([bd3a6a6](https://github.com/davidban77/sonda/commit/bd3a6a667d5b91e6ba1525ba1757713b2bf79f7b))
* **kafka:** use Retry for unknown topic handling ([#60](https://github.com/davidban77/sonda/issues/60)) ([c6f41b0](https://github.com/davidban77/sonda/commit/c6f41b039746a85b63fafb7641baca01607b3a9e))
* move Loki stream labels from sink config to top-level scenario labels ([#61](https://github.com/davidban77/sonda/issues/61)) ([ba49f66](https://github.com/davidban77/sonda/commit/ba49f66e5e360fcf53b630fbb69b8f7d8e51ed7f))


### Documentation

* add Alertmanager integration guide and Docker Compose alerting profile ([#78](https://github.com/davidban77/sonda/issues/78)) ([#123](https://github.com/davidban77/sonda/issues/123)) ([f5afa60](https://github.com/davidban77/sonda/commit/f5afa602d20ec1d347d9b7707f6aabc68bc85a89))
* add long-running scenarios start/stop pattern ([#59](https://github.com/davidban77/sonda/issues/59)) ([d60c221](https://github.com/davidban77/sonda/commit/d60c221ec9339cb69505aae5fd934fd032ecdc04))
* add phase 9 hardening plan from full codebase review ([#104](https://github.com/davidban77/sonda/issues/104)) ([d0e314b](https://github.com/davidban77/sonda/commit/d0e314bbafaa79c52705814579950624a98d314d))
* add sink batching reference and Grafana healthcheck ([#128](https://github.com/davidban77/sonda/issues/128)) ([256aa2a](https://github.com/davidban77/sonda/commit/256aa2a56fdc07d754279aa7a2b47ff81c6fb670))
* apply review findings to tutorial — motivation, transitions, progressive disclosure ([#67](https://github.com/davidban77/sonda/issues/67)) ([1cf5bda](https://github.com/davidban77/sonda/commit/1cf5bda66c0e88e7f4eea3c3cc47fb535bd5abfc))
* backfill missing example files and upgrade guide quality ([#68](https://github.com/davidban77/sonda/issues/68)) ([8f99038](https://github.com/davidban77/sonda/commit/8f990389d1c25ca3881647b78cc74701a48a1a7c))
* comprehensive update for Phase 9A/9B/9C changes ([#122](https://github.com/davidban77/sonda/issues/122)) ([b0152f6](https://github.com/davidban77/sonda/commit/b0152f6f3f1d5750203bae08198b633d40453aca))
* deduplicate Home, Getting Started, and Tutorial pages ([#103](https://github.com/davidban77/sonda/issues/103)) ([9d2f66f](https://github.com/davidban77/sonda/commit/9d2f66f0bd9ce689865c6b72263e23ee9681b8cc))
* enable remote-write/kafka in release builds, improve walkthrough ([#56](https://github.com/davidban77/sonda/issues/56)) ([656c9ca](https://github.com/davidban77/sonda/commit/656c9ca9a6ba90c2417e7ae3a4d50d160848ded4))
* remove walkthrough-reference.md after content migration ([#102](https://github.com/davidban77/sonda/issues/102)) ([6c3d433](https://github.com/davidban77/sonda/commit/6c3d433caa8589b1323e6c67609f7396d1bfafb1))
* rewrite comprehensive walkthrough as focused tutorial ([#64](https://github.com/davidban77/sonda/issues/64)) ([97c2847](https://github.com/davidban77/sonda/commit/97c28475c4232e2a1fe478d943e0a31049492f61))
* slim README, move detailed content to GitHub Pages ([#53](https://github.com/davidban77/sonda/issues/53)) ([99bf855](https://github.com/davidban77/sonda/commit/99bf8556bf7730e4edf0cb0fc91d38f11e48dbfb))
* upgrade doc agent to Technical Writer persona ([#65](https://github.com/davidban77/sonda/issues/65)) ([9c470fd](https://github.com/davidban77/sonda/commit/9c470fdc6d2d1d4b3980b76772c8941b146e6456))


### Miscellaneous

* add critical evaluation rules to implementer and reviewer agents ([#119](https://github.com/davidban77/sonda/issues/119)) ([5b09dd2](https://github.com/davidban77/sonda/commit/5b09dd2b3c60532a0bfd59402b95137de1377b2e))
* add smoke agent for Docker/infra-level end-to-end testing ([#125](https://github.com/davidban77/sonda/issues/125)) ([da6bd48](https://github.com/davidban77/sonda/commit/da6bd4829ad505ab669989cdd2c67acd9bd49b47))
* enforce uv-only Python tooling for agents ([#63](https://github.com/davidban77/sonda/issues/63)) ([c8e04c6](https://github.com/davidban77/sonda/commit/c8e04c690603faa629fbb325cabfe6a05afde267))
* fold tester agent into implementer, streamline pipeline ([#50](https://github.com/davidban77/sonda/issues/50)) ([8a95d03](https://github.com/davidban77/sonda/commit/8a95d030d03629c3b13f3149c41d165a307653e2))
* upgrade implementer and reviewer agent personas ([#66](https://github.com/davidban77/sonda/issues/66)) ([cb56079](https://github.com/davidban77/sonda/commit/cb56079d288497a704c52f52efe04b600f9cdcc9))


### Refactoring

* deduplicate SplitMix64 into shared util module (9D.3) ([#124](https://github.com/davidban77/sonda/issues/124)) ([751064b](https://github.com/davidban77/sonda/commit/751064b66404ad551e5ad0ef7aec1b437f2efb26))
* Phase 9E polish and consistency (slices 9E.1-9E.8) ([#130](https://github.com/davidban77/sonda/issues/130)) ([4093bf6](https://github.com/davidban77/sonda/commit/4093bf60b4cec68c252055aa00be77b0aa562a71))
* unify metric and log runner loops via shared core_loop (9D.1) ([#129](https://github.com/davidban77/sonda/issues/129)) ([5ecfff5](https://github.com/davidban77/sonda/commit/5ecfff5590603b817e1ec6cecf1be144f80b30e3))

## [0.3.0](https://github.com/davidban77/sonda/compare/v0.2.0...v0.3.0) (2026-03-30)


### Features

* add parallel session worktrees to agent workflow ([#48](https://github.com/davidban77/sonda/issues/48)) ([97c51af](https://github.com/davidban77/sonda/commit/97c51af343d6fbcc6bbd3ad5bc2a5cc6cf90659a))
* add static labels support to log events ([#49](https://github.com/davidban77/sonda/issues/49)) ([9621c49](https://github.com/davidban77/sonda/commit/9621c49efbd7a46b85c0740fb681a20157c6e1c4))
* **cli:** add colored status output with --quiet flag ([#46](https://github.com/davidban77/sonda/issues/46)) ([7e42f68](https://github.com/davidban77/sonda/commit/7e42f6833b892597fb3137aa7072eb59f099a23c))


### Bug Fixes

* make kafka an opt-in feature flag instead of always-on ([#44](https://github.com/davidban77/sonda/issues/44)) ([1bf3fe1](https://github.com/davidban77/sonda/commit/1bf3fe137aee499193618585a461d946a94ab95a))


### Documentation

* add comprehensive walkthrough and improvement recommendations ([#38](https://github.com/davidban77/sonda/issues/38)) ([72d718e](https://github.com/davidban77/sonda/commit/72d718efd86b83651551c87849b9b8d300a337a0))
* add worktree cleanup rules to CLAUDE.md ([#39](https://github.com/davidban77/sonda/issues/39)) ([aa8a327](https://github.com/davidban77/sonda/commit/aa8a3272ad31fedf2e6807b160367674a30e4c4c))


### Miscellaneous

* add feature branch workflow for agent pipeline ([#47](https://github.com/davidban77/sonda/issues/47)) ([49b6462](https://github.com/davidban77/sonda/commit/49b6462b603fe8158b025b705b268acee3c8039c))


### CI/CD

* bump actions/setup-python from 5 to 6 ([#41](https://github.com/davidban77/sonda/issues/41)) ([62c628c](https://github.com/davidban77/sonda/commit/62c628c8d0a9ea138f557bc9201445150e14ef94))

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
