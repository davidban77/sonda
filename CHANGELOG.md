# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

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
