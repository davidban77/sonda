# sonda-core — The Engine

This is the library crate. It owns **all** domain logic. If it generates signals, schedules events,
encodes data, or delivers output — it lives here.

## Module Layout

```
src/
├── lib.rs              ← public API surface, re-exports, SondaError + sub-enums
│                          (ConfigError, GeneratorError, EncoderError, RuntimeError)
├── emit.rs             ← synchronous single-event helpers: emit_log, emit_metric.
│                          Check the encoder/sink pairing (emit_log also refuses the
│                          gnmi encoder and gnmi_target sink), build a one-shot encoder +
│                          sink, encode one event, write (write_metric_event for a sink
│                          that opts in), flush, drop. A gnmi_target sink JOINS the live
│                          target on its address and never binds; no live target is a
│                          ConfigError. A joined write only overwrites a path the target
│                          already serves and owns nothing; an unserved path is a
│                          ConfigError. Used by the sonda-server `POST /events` handler.
├── util.rs             ← pub(crate) shared utility functions (splitmix64 deterministic hash;
│                          constant_time_eq under gnmi: length check, then an XOR-fold over
│                          every byte pair; ensure_crypto_provider under kafka or gnmi:
│                          installs rustls's ring provider as the PROCESS-WIDE default when
│                          none is, for every rustls config in the process, embedder's
│                          included. Release builds compile ring alone; a workspace build
│                          also compiles aws-lc-rs, so rustls cannot pick one by itself)
├── packs/
│   ├── extend.rs       ← materialize(extension, base) -> MetricPackDef: the pure pre-pass that
│   │                      folds one `extends:` link. `metrics:` is additive and refuses a
│   │                      selector the base declares; `deviations:` is the only way to change
│   │                      one, each giving exactly one of `replace:` (field-wholesale) or
│   │                      `not_supported:` (removes). The extension's shared_labels are merged
│   │                      over the base's HERE, which is why the label precedence chain has no
│   │                      extension case in it. Chain walking lives in compiler/expand.rs
│   │                      (resolve_pack_chain) because that is where the resolver is.
│   └── mod.rs          ← metric pack engine: MetricPackDef, MetricSpec (name + optional id),
│                          PackScenarioConfig, MetricOverride, expand_pack().
│                          validate_pack() is the ONE definition of addressability: a metric
│                          name is unique within a pack, or every spec sharing it declares a
│                          unique `id:`; names may not contain `.`. It also refuses specs
│                          sharing a name with different `gnmi:` or `snmp:` hints, and a
│                          hint path that is not a PathTemplate (gnmi feature) or a hint
│                          OID that is not an OidTemplate (snmp feature). An invalid pack does not
│                          load, so MetricSelector/resolve_selector never meet an ambiguous
│                          one and do not re-derive the rule.
│                          MetricSpec.gnmi: Option<GnmiHint { path, value }> (deny_unknown_fields;
│                          ungated). fold_gnmi_hints() is the ONE fold of hints into a gnmi
│                          encoder's paths:/values: (an existing entry wins, each map on its
│                          own; any other encoder unchanged), called by expand_pack and by
│                          compiler/expand.rs once per pack entry. MetricSpec.snmp:
│                          Option<SnmpHint { oid, type }> and fold_snmp_hints(&mut encoder,
│                          specs), the same rules for an snmp encoder's oids:/types:, applied
│                          right after the gnmi fold at both call sites. resolve_override_keys() is the
│                          ONE definition of override keying — v1 expand_pack and the v2
│                          compiler both call it, so they cannot drift; it also refuses two
│                          keys reaching one spec, which addressability alone permits (a
│                          unique name that declares an id answers to both name and name.id).
│                          Engine types and expansion only.
├── catalog/
│   ├── mod.rs          ← catalog enumeration and `@name` resolution: CatalogEntry, EntryKind,
│   │                      EntryOrigin, SkipReason/SkippedFile/CatalogListing, header_entry()
│   │                      (the ONE classifier both sources go through), enumerate(),
│   │                      enumerate_with_skips(), merged(), CatalogPackResolver — the
│   │                      `--catalog <dir>` directory chained in front of the builtins.
│   │                      A composable entry that would not resolve carries the reason in
│   │                      CatalogEntry::pack_error, computed by the same parse+validate_pack
│   │                      the resolver runs, so a listing and a run cannot disagree. Marked,
│   │                      not skipped: `sonda show` is how the user reads the broken file.
│   └── builtin.rs      ← the packs compiled into the binary via include_str!, reached
│                          through the `sonda-core/packs` symlink to the repo-root copy —
│                          cargo publish packages only what is under the crate root.
│                          PACK_COUNT is declared beside the list and gated: a half-wired
│                          embedded set fails rather than listing fewer packs
│                          successfully. BuiltinPackResolver.
├── analysis/
│   └── pattern.rs      ← time-series pattern detection: detect_pattern() → Pattern enum
│                          (Steady / Spike / Climb / Sawtooth / Flap / Step). Pure statistics,
│                          consumed by `sonda new --from <csv>`.
├── model/
│   ├── mod.rs          ← module declarations
│   ├── metric.rs       ← ValidatedMetricName (newtype over Arc<str>, validates once at construction),
│   │                      MetricEvent (ValidatedMetricName name, Arc<Labels>), Labels, from_parts().
│   │                      MetricKey = (ValidatedMetricName, Arc<Labels>), the one series key
│   │                      (re-exported from schedule::stats).
│   │                      Labels::iter() returns (&str, &str). Labels::new() is #[cfg(test)] pub(crate).
│   └── log.rs          ← LogEvent (with Labels support for scenario-level static labels).
│                          Severity has explicit Ord/PartialOrd (rank-based, not derived from variant order).
├── generator/
│   ├── mod.rs          ← ValueGenerator trait + factory, CsvColumnSpec (multi-column csv_replay),
│   │                      GeneratorConfig enum (core types + 6 operational aliases:
│   │                      flap, saturation, leak, degradation, steady, spike_event).
│   │                      Aliases are pure syntactic sugar — desugared before create_generator().
│   ├── constant.rs
│   ├── uniform.rs
│   ├── sine.rs
│   ├── sawtooth.rs
│   ├── sequence.rs     ← explicit value sequence (incident pattern modeling)
│   ├── step.rs         ← monotonic step counter with optional wrap-around (rate/increase testing)
│   ├── spike.rs        ← baseline with periodic spikes (anomaly/alert testing)
│   ├── jitter.rs       ← JitterWrapper: adds deterministic uniform noise to any ValueGenerator
│   ├── csv_header.rs   ← CSV header parsing for Grafana-style label-aware column headers
│   ├── csv_replay.rs   ← CSV file-based replay for metric values
│   ├── histogram.rs    ← HistogramGenerator (cumulative bucket counts, Distribution, to_distribution)
│   ├── summary.rs      ← SummaryGenerator (quantile values via sorted observations)
│   ├── log_template.rs ← template-based log line generator
│   └── log_csv_replay.rs ← CSV-replay log generator (structured timestamp/severity/message + fields)
├── schedule/
│   ├── mod.rs          ← LaunchId (pub(crate): one per launch_prepared (run_multi, the
│   │                      CLI) / launch_multi_compiled call, and per launch_scenario
│   │                      (_with_gates) / create_sink call for a launch of one; reaches
│   │                      sinks through create_sink_in),
│   │                      GapWindow, BurstWindow, CardinalitySpikeWindow, is_in_spike,
│   │                      DynamicLabel (always-on rotating label, label_value_for_tick()),
│   │                      ParsedSchedule (parses BaseScheduleConfig into resolved Duration values)
│   ├── core_loop.rs    ← pub(crate) shared schedule loop (run_schedule_loop, TickFn, TickContext,
│   │                      TickResult). Owns all rate control, gap/burst/spike window handling,
│   │                      stats tracking, and shutdown. Signal runners provide a TickFn closure.
│   │                      The tick and gap sleeps select on cancel; a cancel exits through
│   │                      the loop's cancel check without emitting a tick.
│   ├── stats.rs        ← ScenarioStats (live telemetry + current_values map: one entry per series for scrape endpoints)
│   ├── handle.rs       ← ScenarioHandle (lifecycle: stop, join, elapsed, stats_snapshot;
│   │                      recovers from poisoned stats lock instead of panicking)
│   ├── launch.rs       ← validate_entry + prepare_entries + launch_scenario (unified launch API,
│   │                      PreparedEntry, shared expand→validate→phase_offset pipeline).
│   │                      launch_prepared launches a file's entries as one launch, ids
│   │                      `{prefix}-{i}`, failing fast (stops what it launched).
│   │                      validate_file is the ONE home of cross-entry rules, run by
│   │                      prepare_entries and the gated path: gnmi_target entries on one
│   │                      address agree on settings, a wildcard and another address never
│   │                      share a port, and every gnmi encoder agrees with its entries.
│   │                      Its leaf check gives a series one leaf (keys of labels used only
│   │                      as {x?} that no event carries left out) and the set of keys it may
│   │                      leave out (a spike over nothing); two elements meet when each key
│   │                      is on both with values that can be equal, or every side that has
│   │                      it may leave it out. Per pair O(elements x keys), never 2^k shapes.
│   │                      check_running_sinks() checks entries against sinks already
│   │                      running in the process (gnmi_target via registry().check());
│   │                      both multi-launch paths call it (POST /scenarios through
│   │                      launch_multi_compiled).
│   ├── runner.rs           ← metric event loop: builds generator/encoder/labels, delegates to core_loop
│   ├── log_runner.rs       ← log event loop: builds log generator/encoder/labels, delegates to core_loop
│   ├── histogram_runner.rs ← histogram event loop: pre-built Arc<Labels> per bucket, delegates to core_loop
│   ├── summary_runner.rs   ← summary event loop: pre-built Arc<Labels> per quantile, delegates to core_loop
│   └── multi_runner.rs     ← concurrent multi-scenario runner (run_multi, respects phase_offset per entry)
├── encoder/
│   ├── mod.rs          ← Encoder trait + factory
│   ├── prometheus.rs   ← Prometheus text exposition format
│   ├── influx.rs       ← Influx Line Protocol (post-MVP)
│   ├── json.rs         ← JSON Lines (post-MVP)
│   ├── otlp.rs         ← OTLP protobuf: hand-written prost structs for metrics + logs,
│   │                      OtlpEncoder (Metric/LogRecord), parser helpers (feature = "otlp")
│   ├── remote_write.rs ← Prometheus remote write protobuf (feature = "remote-write")
│   ├── gnmi/           ← gNMI Notification encoder (feature = "gnmi"):
│   │   ├── mod.rs      ←   GnmiEncoder, GnmiEncoderConfig (deny_unknown_fields; drop_labels).
│   │   │                   Path bytes cached per series on first sight; a series is the
│   │   │                   metric name and the labels its template sees (all but the
│   │   │                   target_label and drop_labels, unless the template references
│   │   │                   them), found by a DefaultHasher fingerprint of those, then
│   │   │                   compared label by label: a hit allocates nothing. A path
│   │   │                   rejection (uncovered label, missing placeholder label, invalid
│   │   │                   element name) is cached as its text, and later events get it
│   │   │                   back without a render (one allocation: the text). A value
│   │   │                   rejection is per event and not cached.
│   │   │                   uncovered_label + template_for are the ONE coverage and
│   │   │                   template rules validation also calls. Output is one Notification
│   │   │                   per call, NOT length-prefixed; validate_encoder_sink_pairing
│   │   │                   pairs it with the gnmi_target sink only, in both directions.
│   │   │                   Metrics, histogram and summary entries take it; validation
│   │   │                   checks each series an entry emits (histogram: <name>_bucket
│   │   │                   with `le`, _sum, _count; summary: <name> with `quantile`,
│   │   │                   _sum, _count). The `le`/`quantile` template must reference
│   │   │                   the key and may not drop it; _sum/_count need a template
│   │   │                   without it: their own paths: entries, path when the
│   │   │                   bucket/quantile series has its own paths: entry, or a path
│   │   │                   that writes it optionally ([le={le?}]). A cardinality_spikes
│   │   │                   label is a template's only as an optional key. Logs
│   │   │                   entries refuse both encoder and sink. encode_delete (pub(crate)) writes a
│   │   │                   Notification deleting a full path: origin and target on the prefix, the
│   │   │                   elements on delete[0], split as an update's prefix and path are.
│   │   ├── value.rs    ←   GnmiValueType and its YAML shape (hand-written Deserialize: enum
│   │   │                   codes as ints or integer strings; enum labels as any scalar,
│   │   │                   stored as text). Compiled WITHOUT the gnmi feature too: without it,
│   │   │                   encoder/mod.rs declares an inline `gnmi` module holding only this
│   │   │                   file, so crate::encoder::gnmi::GnmiValueType names it in every
│   │   │                   build. mod.rs re-exports it.
│   │   ├── path.rs     ←   PathTemplate (parse/placeholders/render), parse_client_path;
│   │   │                   placeholders() yields Placeholder { name, optional }. An
│   │   │                   optional key value [key={x?}] renders the key only when the
│   │   │                   event carries x (render leaves it out; never an error); an
│   │   │                   optional element name is a parse error.
│   │   │                   is_valid_element_name is the ONE element-name rule that
│   │   │                   render and validation (every static, dynamic and spike
│   │   │                   value an element-name placeholder can render to) both apply
│   │   └── proto.rs    ←   hand-written prost subset of gnmi.proto: Path, Notification,
│   │                       the Capability, Get and Subscribe request/response messages
│   ├── snmp/           ← SNMP encoder (feature = "snmp"):
│   │   ├── mod.rs      ←   SnmpEncoder, SnmpEncoderConfig (deny_unknown_fields; oids:,
│   │   │                   types:, agent_label "device", drop_labels). Output is a PUBLISH
│   │   │                   RECORD, not a bare VarBind: one byte (the template's
│   │   │                   index_arcs, 1 for a scalar; RECORD_HEADER = 1), then a BER
│   │   │                   VarBind SEQUENCE { OID, value }. The instance OID is rendered
│   │   │                   once per series and cached as its encoded OBJECT IDENTIFIER
│   │   │                   (fingerprint, then label-by-label compare, as gnmi's path
│   │   │                   cache: agent_label and drop_labels are unseen unless
│   │   │                   referenced); a rejection is cached as its text (one
│   │   │                   allocation per later event). A value rejection is per event,
│   │   │                   not cached, and truncates what the record wrote. uncovered_label
│   │   │                   (free fn over the config) is the ONE coverage rule validation
│   │   │                   also calls. Metrics only; validation pairs it with the
│   │   │                   snmp_agent sink both ways.
│   │   ├── ber.rs      ←   pub(crate) BER subset: Tag, BerError (incl. UnsupportedVersion),
│   │   │                   definite lengths (write_tlv back-patches, long form above 127),
│   │   │                   write_i64/write_u64 and the typed writers the value layer uses
│   │   │                   (write_integer(i32), write_counter32/gauge32/timeticks(u32),
│   │   │                   write_counter64(u64), write_string), write_oid, write_varbind;
│   │   │                   Reader, Message::decode (version read first: anything but 0/1
│   │   │                   is UnsupportedVersion before the community is parsed),
│   │   │                   VarBinds/VarBind, message_len/write_message. Fixtures are
│   │   │                   inline constants in its tests, named by tool, flags and version.
│   │   ├── oid.rs      ←   Oid (Vec<u32>, lexicographic Ord = walk order, ≥ 2 arcs and an
│   │   │                   encodable first pair, Borrow<[u32]>, BER body helpers);
│   │   │                   index_arc() is the ONE label-value → arc rule (canonical
│   │   │                   decimal u32: no sign, no leading zero), applied by render and
│   │   │                   validation; OidTemplate (pub(crate)): object OID then whole
│   │   │                   trailing {label} arcs, or a scalar's literal .0 (a template
│   │   │                   with no placeholder must end in .0; its object() excludes it
│   │   │                   and index_arcs() is 1), object()/index_arcs()/placeholders()/
│   │   │                   render()/arcs() (TemplateArc: Literal or Label, in order)/
│   │   │                   can_render_under() (the system-group rule);
│   │   │                   {x:str} is Unsupported; at most 128 arcs.
│   │   └── value.rs    ←   SnmpValueType (integer, counter32, counter64, gauge32 = default,
│   │                       unsigned32, timeticks, string; non_exhaustive). Compiled WITHOUT
│   │                       the snmp feature too, through an inline `snmp` module in
│   │                       encoder/mod.rs, as gnmi's value.rs is. write() (snmp only)
│   │                       rounds halves away from zero and refuses non-finite or
│   │                       out-of-range values (i32 / u32 / u64) before writing anything;
│   │                       string writes the shortest round-trip text, NaN/inf/-inf too.
│   └── syslog.rs       ← RFC 5424 syslog format (log-only)
├── sink/
│   ├── mod.rs          ← Sink trait + factory. SinkConfig::SnmpAgent { listen } (snmp
│   │                      feature) / SnmpAgentDisabled {}: validation pairs it with the
│   │                      snmp encoder; create_sink refuses it with a ConfigError.
│   ├── stdout.rs       ← BufWriter<Stdout>
│   ├── file.rs         ← BufWriter<File>
│   ├── tcp.rs          ← TCP socket (BufWriter<TcpStream>)
│   ├── udp.rs          ← UDP socket (UdpSocket)
│   ├── http.rs         ← HTTP push sink (ureq, feature = "http")
│   ├── loki.rs         ← Loki log push sink (HTTP, ureq, feature = "http")
│   ├── remote_write.rs ← Prometheus remote write sink (batches TimeSeries, snappy, feature = "remote-write")
│   ├── channel.rs      ← in-memory channel sink (mpsc::Sender<Vec<u8>>, for testing)
│   ├── memory.rs       ← in-memory buffer sink (Vec<Vec<u8>>, for testing and embedding)
│   ├── kafka.rs        ← Kafka producer (rskafka, feature = "kafka")
│   ├── listener_registry.rs ← ListenerRegistry<T: ListenerTarget> (feature = "gnmi";
│   │                      pub(crate)): address → Weak<T> + T::SocketState. acquire() binds
│   │                      (T::bind) or joins; a target whose listener task exited is not
│   │                      live (is_live); before binding it waits (≤ 2 s in all, never
│   │                      under the bind lock) for a stopped target's socket on an
│   │                      overlapping address to close (socket_open / closed). join()
│   │                      never binds and finds a live wildcard target covering the
│   │                      address. Differing settings (T::serves), or a second listener
│   │                      contending for a port (listen_overlap: same address, or a
│   │                      wildcard covering the other's family; IPv4-mapped addresses
│   │                      compare as IPv4), are a ConfigError; port 0 always binds a new
│   │                      target. check() answers as acquire() would, binding nothing.
│   │                      targets() lists the live targets sorted by local_addr.
│   │                      listen_overlap assumes a dual-stack [::].
│   ├── grpc_codec.rs   ← ProstCodec<T, U>: tonic codec over prost messages (otlp_grpc,
│   │                      gnmi Capabilities); compiled with "otlp" or "gnmi".
│   │                      PreEncoded(Bytes) + PreEncodedCodec<U> ("gnmi" only): sends
│   │                      already-encoded responses verbatim (gnmi Subscribe).
│   ├── gnmi/           ← gnmi_target sink (feature = "gnmi"):
│   │   ├── mod.rs      ←   GnmiTargetConfig (deny_unknown_fields; settings() is the ONE
│   │   │                   field check validation and the sink both call; it returns
│   │   │                   an IPv4-mapped listen as the IPv4 address it maps).
│   │   │                   tls: GnmiTlsConfig { cert, key, client_ca } and credentials:
│   │   │                   GnmiCredentials { username, password }, both deny_unknown_fields;
│   │   │                   the password is writeOnly in the schema and Debug prints "***".
│   │   │                   GnmiCredentials' Deserialize is hand-written (deserialize_any
│   │   │                   over credentials_fields::GnmiCredentials, a private derive of the
│   │   │                   same name, so its errors name GnmiCredentials): a value that is
│   │   │                   not a map is refused
│   │   │                   with a fixed message, because a buffered sink block answers
│   │   │                   deserialize_struct on a scalar with an error quoting it.
│   │   │                   settings() refuses credentials without tls, and reads each tls
│   │   │                   file into TlsIdentity (paths + PEM bytes; Debug prints the
│   │   │                   paths): every file parsed on its own (rustls-pki-types) so an
│   │   │                   error names field and file, then tonic's server TLS config built
│   │   │                   once, so a key that is not cert's fails at validation, not bind.
│   │   │                   server_config() calls util::ensure_crypto_provider: tonic's
│   │   │                   server TLS reads the process default provider.
│   │   │                   serves() compares tls by its paths and credentials by both fields;
│   │   │                   beside() names credentials by username only.
│   │   │                   GnmiTargetSink: wants_metric_events() = true, publishes
│   │   │                   synchronously from write_metric_event; write() is an error.
│   │   │                   Keeps one Publication { id, path, buffers } per path it
│   │   │                   published (`published`), each MetricKey pointing at its entry: the
│   │   │                   first event of a key decodes its path, a second key on a
│   │   │                   known path shares the entry, so an id that changes changes
│   │   │                   for all. A write is delivered once upserted. A sink's Owner
│   │   │                   is its launch (LaunchId): sinks of one launch share a path,
│   │   │                   another launch's path is a ConfigError at the first write.
│   │   │                   Drop releases each entry's id once (joined sinks have none).
│   │   │                   A joined sink (join()) owns nothing: overwrite() only, of a copy.
│   │   │                   PublishBuffers: two BytesMut per publication used in turn; each
│   │   │                   notification is copied into the one used two notifications
│   │   │                   earlier (try_reclaim, extend, split().freeze()), which reuses
│   │   │                   its allocation unless a subscriber still holds the Bytes last
│   │   │                   frozen from it, and allocates once then.
│   │   ├── target.rs   ←   GnmiTarget: per-address latest-value table keyed by the
│   │   │                   RENDERED PATH (PathKey: origin, target, elems; its hash is
│   │   │                   computed once in new() and Hash writes that u64, Eq compares
│   │   │                   the hash, then the path, so table growth rehashes no
│   │   │                   path). A series has one Owner, a launch: only its sinks
│   │   │                   publish it (an upsert).
│   │   │                   Each slot counts its publishers: a publish that reaches the
│   │   │                   write lock (no id, or a cached id that no longer names its
│   │   │                   path) is a publisher's first of that path (+1),
│   │   │                   release() is −1 at drop; the series leaves at zero, not when
│   │   │                   idle. A path with no live publisher is free to any launch.
│   │   │                   A sink caches the table's PathKey (path_of) so siblings
│   │   │                   compare by pointer.
│   │   │                   overwrite() takes nothing. publish()
│   │   │                   locates the value (value_span), refusing bytes without one on
│   │   │                   every event, stores the Bytes it is given (no copy), keeps the value's
│   │   │                   range in them, and compares it with the previous value:
│   │   │                   Published { receivers, changed } (the timestamp is not part of
│   │   │                   the value; a first publish is a change) is the one definition
│   │   │                   of "changed". Only a change with a receiver is broadcast (the
│   │   │                   one Bytes clone). A slot's ValueKind is its first publish's.
│   │   │                   Latest.json is the value as JSON_IETF (json_ietf_value), made
│   │   │                   by the first GnmiTarget::json_ietf_value after the value
│   │   │                   changed, under the series' lock; a change clears it, an
│   │   │                   unchanged publish keeps it, and a publisher never makes it.
│   │   │                   json_ietf_value(id, version, latest) answers from the cache
│   │   │                   when the slot holds `version`, and makes it from `latest`
│   │   │                   without caching when the slot moved on; None for an unknown
│   │   │                   id. json_ietf_of() splices that value into the caller's
│   │   │                   `latest` (with_value), so each response keeps its own
│   │   │                   timestamp.
│   │   │                   Each change raises the series' version by one (1 for its
│   │   │                   first value), set with the value under the series' lock;
│   │   │                   SeriesUpdate and SeriesSnapshot carry it (both
│   │   │                   #[non_exhaustive], as is ValueKind).
│   │   │                   The broadcast carries SeriesEvent (pub(crate), #[non_exhaustive]):
│   │   │                   Changed(SeriesUpdate), or Removed { series_id, bytes } when release()
│   │   │                   removes a slot and a receiver exists: the delete (delete_notification,
│   │   │                   stamped now) is encoded once and sent under the table write lock, so no
│   │   │                   receiver gets a value for the series after its delete. A series leaves
│   │   │                   with its launch's last publisher and never on idle.
│   │   │                   Never awaits. Store and broadcast happen under the series'
│   │   │                   lock, so receivers see a series' updates in table order.
│   │   │                   Only ON_CHANGE (and TARGET_DEFINED) lists hold a receiver
│   │   │                   (subscribe()); every subscription holds a permit of the
│   │   │                   target's max_subscribers Semaphore (admit());
│   │   │                   subscriber_count() is permits taken. The subscriber table
│   │   │                   (Arc<Subscribers>: Mutex<HashMap<id, SubscriberInfo>> and the
│   │   │                   stalled count): register(peer, mode, encoding) enters a
│   │   │                   subscription and returns a SubscriberRecord whose drained() sets
│   │   │                   last_drained, stalled() sets stalled and counts it, and whose
│   │   │                   Drop removes the entry. subscribers() is a snapshot sorted by
│   │   │                   since. get_requests() (count_get_request, an atomic),
│   │   │                   stalled_subscribers_total(), tls(), requires_credentials(),
│   │   │                   max_subscribers() are public. snapshot() shares each
│   │   │                   slot's PathKey and Bytes, copying no path. The server pings an HTTP/2
│   │   │                   connection silent for KEEPALIVE_INTERVAL (30 s) and closes it
│   │   │                   after KEEPALIVE_TIMEOUT (20 s) unanswered; TargetSettings
│   │   │                   carries both so tests can shorten them (no config). Accepted
│   │   │                   sockets get TCP_NODELAY; an accept error other than a
│   │   │                   refused/aborted/reset connection backs off 100 ms. The
│   │   │                   listener task owns a StoppedOnDrop guard, so the target
│   │   │                   reads Stopped however the task ends (panic, runtime drop);
│   │   │                   is_serving() is state == Listening. bind() builds the tonic
│   │   │                   Server (keepalives, and tls_config from TlsIdentity when set,
│   │   │                   timing out a handshake after TLS_HANDSHAKE_TIMEOUT (10 s), carried
│   │   │                   in TargetSettings so tests can shorten it; no config)
│   │   │                   before binding the socket; a TLS error is SondaError::Sink naming
│   │   │                   the address and binds nothing. serve_with_incoming does the TLS
│   │   │                   handshake on each accepted Connection and drops failed ones.
│   │   │                   Teardown: dropping the last Arc cancels the
│   │   │                   root token; the listening socket closes at once (Incoming
│   │   │                   ends), connections still open after DRAIN_GRACE are closed
│   │   │                   (Connection reads EOF), state Listening → Draining → Stopped.
│   │   ├── registry.rs ←   registry(): the process-wide gnmi instance of
│   │   │                   ListenerRegistry<GnmiTarget> (sink/listener_registry.rs), behind
│   │   │                   GnmiTargetRegistry, whose pub surface (acquire, check, join, len,
│   │   │                   is_empty, targets) parses the config with settings() and
│   │   │                   delegates. join() never binds (emit_metric) and compares only
│   │   │                   the settings it sets (tls and credentials included).
│   │   ├── matcher.rs  ←   SubscriptionMatcher: a SubscriptionList's (new()) or a
│   │   │                   GetRequest's (from_paths(prefix, paths, encoding)) paths joined to the
│   │   │                   prefix by join(), the ONE path rule both use (INVALID_ARGUMENT
│   │   │                   for an empty element name or a `...` with keys; from_paths
│   │   │                   also for no paths); paths() returns them joined, in request
│   │   │                   order. matches() aligns the requested elements with the
│   │   │                   series' first ones (aligns(): `*` is any one element, its keys
│   │   │                   still apply; `...` any run of zero or more, tried shortest
│   │   │                   first and lengthened on a mismatch; no allocation), a key of "*"
│   │   │                   or an absent key matching any value, an empty origin or target
│   │   │                   matching any. served_encoding() is the ONE encoding rule
│   │   │                   Subscribe and Get apply: PROTO and JSON (encoding 0, also an
│   │   │                   unset encoding) served as PROTO, JSON_IETF as JSON_IETF;
│   │   │                   BYTES and ASCII UNIMPLEMENTED, named; an unknown number
│   │   │                   INVALID_ARGUMENT. Both constructors apply it first and keep
│   │   │                   the result: encoding() is the served encoding, which Get,
│   │   │                   the list's Delivered and the subscriber table read, and
│   │   │                   encoding_name() its gnmi.proto name. new() also refuses (INVALID_ARGUMENT) an
│   │   │                   empty list, an unknown list mode, or (STREAM lists only) an
│   │   │                   unknown subscription mode; STREAM, ONCE and POLL are
│   │   │                   accepted. pub(crate).
│   │   ├── test_tls.rs ←   (cfg(test)) TlsFiles: a CA, and a server (localhost, 127.0.0.1) and
│   │   │                   a client certificate it signed, made with rcgen (a dev-dependency)
│   │   │                   and written to a temp dir; tls()/mutual_tls() blocks, a TLS channel,
│   │   │                   and once_syncs() for the TLS tests.
│   │   ├── notification.rs ← value_span(): the byte range of an encoded Notification's
│   │   │                   first update's val (the TypedValue without key and length)
│   │   │                   and its ValueKind (the first field's number); walks keys and
│   │   │                   lengths, decodes no path, allocates nothing.
│   │   │                   json_ietf_value(): that val as a TypedValue { json_ietf_val },
│   │   │                   one allocation; json_text is the ONE RFC 7951 rendering: uint
│   │   │                   and int as JSON strings, double as a JSON number (NaN and
│   │   │                   the infinities as the strings "NaN", "Infinity",
│   │   │                   "-Infinity"), bool as true/false, string as an escaped JSON
│   │   │                   string, a json_ietf_val as it is; any other kind is None.
│   │   │                   with_value(): the notification with its first update's val
│   │   │                   replaced and every other field copied, one allocation.
│   │   │                   pub(crate).
│   │   └── service.rs  ←   GnmiService: tower::Service router served with
│   │                       serve_with_incoming (never add_service). Holds the target
│   │                       WEAKLY (so does every handler and subscription task): nothing a
│   │                       client does keeps a target alive; a request after it is gone is
│   │                       UNAVAILABLE. When the target's settings carry credentials,
│   │                       the router checks the `username` and `password` metadata of
│   │                       every request before dispatch (authenticated(): both compared
│   │                       with constant_time_eq); a missing or wrong pair is
│   │                       UNAUTHENTICATED "credentials required", logged once at warn with
│   │                       the peer (TCP or TLS connect info) and the method, never a value.
│   │                       Capabilities: 0.10.0, [PROTO, JSON_IETF], no models.
│   │                       Get (Grpc::unary, PreEncodedCodec<GetRequest>): every call
│   │                       the router dispatches counts in target.get_requests(), served or
│   │                       refused (the credential check runs first). get() answers
│   │                       from one snapshot of the table, no subscriber place taken:
│   │                       every series a from_paths matcher matches, once, as its stored
│   │                       notification (PROTO) or json_ietf_of (JSON_IETF; a None is
│   │                       skipped), framed by get_response() as GetResponse.notification
│   │                       (field 1, the same framing as SubscribeResponse.update:
│   │                       put_notification). ALL, STATE, OPERATIONAL served; CONFIG an
│   │                       empty response; unknown data type INVALID_ARGUMENT; no series
│   │                       matched NOT_FOUND "no series matches <first joined path>".
│   │                       Subscribe: first message must be a SubscriptionList; start()
│   │                       plans it, takes a place with target.admit() (or the request
│   │                       is RESOURCE_EXHAUSTED; the producer holds it until it returns),
│   │                       registers it (target.register(peer, mode_name(list),
│   │                       encoding_name(matcher.encoding())); mode_name: ONCE, POLL,
│   │                       STREAM/<mode>, STREAM/MIXED;
│   │                       the producer holds the SubscriberRecord: Step::Sent calls
│   │                       drained(), Step::Stalled calls stalled() then the
│   │                       after_stall_marked probe hook before it returns), takes
│   │                       the broadcast receiver, and builds the producer, which
│   │                       subscribe() then spawns. One task (read_requests) reads the
│   │                       rest of the inbound stream (never polled in a select!): for a
│   │                       POLL list it forwards each poll on an mpsc of 1 made by
│   │                       Producer::poll_requests(), and ignores everything else; for
│   │                       any other list it ignores everything, and its end does not end
│   │                       the subscription. A producer
│   │                       task sends the initial sync (matching snapshot, none with
│   │                       updates_only, then sync_response). A POLL list then takes a
│   │                       poll only while its queue is empty (select! arm guarded by
│   │                       pending.is_empty()) and answers it with poll_batch(): every
│   │                       matching series' latest through queue_value, not indexed
│   │                       (no deletes, no suppression), then sync_response; updates_only
│   │                       is ignored there. When the reader's sender drops (half-close)
│   │                       the list ends OK once its queue is empty (ends_when_drained).
│   │                       ONCE and POLL plan no groups, so their stall_limit is the floor,
│   │                       and it runs only while responses wait. plan_groups splits a STREAM
│   │                       list: SAMPLE into sample groups; ON_CHANGE into the ON_CHANGE
│   │                       set plus a heartbeat group when heartbeat_interval > 0;
│   │                       TARGET_DEFINED into both, the set and heartbeat for string_val
│   │                       series (KindFilter::Strings), a sample group for the rest
│   │                       (NonStrings). Periods are max(interval, sample_floor), one group
│   │                       per (period, kinds, suppress); next_due skips missed periods in
│   │                       one step. SAMPLE with suppress_redundant: a group whose sent map
│   │                       (SeriesId -> version) is seeded
│   │                       by the initial sync; due() sends a series only when its version differs
│   │                       from the recorded one and reports each recorded series gone from the
│   │                       snapshot, which sample() deletes through the index; its heartbeat_interval adds
│   │                       a heartbeat group. Without suppress_redundant SAMPLE's heartbeat_interval
│   │                       is not read; TARGET_DEFINED's sample groups never suppress. The set's broadcast
│   │                       receiver is taken before the initial snapshot; a series is
│   │                       matched once, when first seen (snapshot path, or PathKey::decode
│   │                       of the update), cached by SeriesId (Seen { path, queued }: path is
│   │                       Some only for a matched series) and pruned to the table past 2 x its
│   │                       live size (at least 1024). queued is the version of the value last queued for the
│   │                       series, by the initial sync, a resync, a heartbeat or a change;
│   │                       offer() drops a change whose version is not newer, so a change
│   │                       racing a snapshot is sent once. Heartbeats are never filtered
│   │                       and set queued. Delivered is the list's delivery index: PathKey ->
│   │                       SeriesId for every path queued a value and not yet a delete, and
│   │                       SeriesId -> Option<PathKey> (None: deleted, its Removed unread).
│   │                       Every value goes through Delivered::queue_value, as one Queued { id,
│   │                       version, path, latest, placement } (Placement::InPlace for sync,
│   │                       resync, heartbeat and change; Last for samples): a path
│   │                       indexed under another id gets that id's delete first. A JSON_IETF
│   │                       list's Delivered holds the target (weakly) and queues each value as
│   │                       target.json_ietf_of(id, version, latest) returns it, nothing when
│   │                       that is None; a PROTO list's queues the stored bytes. Indexed: values
│   │                       of series the set matches or a suppressing group sends; never a value
│   │                       only a plain SAMPLE sends. queue_delete deletes only an indexed id;
│   │                       with a receiver the id is then remembered as deleted. Removed of an
│   │                       indexed id queues the broadcast's bytes; of a remembered id, nothing
│   │                       (and forgets it). offer() skips a remembered id and a path indexed
│   │                       under another id (that change predates the indexed series). The
│   │                       prune on a change and every resync sweep(): delete each indexed id
│   │                       absent from the snapshot. Pending::push_delete
│   │                       drops a queued value for it and queues the delete last. Lagged(n) moves the set to a
│   │                       receiver at the broadcast's tail (resubscribe(), dropping the
│   │                       backlog it fell behind on), sweeps, then re-queues the latest
│   │                       of every matched series (no second sync marker; debug log with
│   │                       n); forget_after(rx.len()) forgets every remembered delete once the
│   │                       new receiver has read what it held at the snapshot (read_one counts). Pending queues encoded responses (sync marker, samples) and
│   │                       series values, at most one per series: a newer change,
│   │                       heartbeat or resync replaces it in place. Each stored
│   │                       Notification is framed as SubscribeResponse.update bytes. Its
│   │                       select! arms: listener left Listening (state()), tx.closed(),
│   │                       tx.reserve() (mpsc of 64: a client that stops reading parks only
│   │                       its own task) under a deadline stall_limit after the queue last
│   │                       became non-empty or last sent, the broadcast (always read), the
│   │                       next sample (only while no encoded response waits), the next
│   │                       heartbeat. stall_limit = max(3 x the list's longest sample period,
│   │                       stall_floor); heartbeats do not count. stall_floor is
│   │                       STALL_FLOOR (10 s), carried in TargetSettings so tests can
│   │                       shorten it (no config). Responses waiting a whole stall_limit
│   │                       with none taken from the queue end the subscription UNAVAILABLE
│   │                       ("subscriber stalled: no data drained for ..."), logged at warn
│   │                       with the peer address (or "unknown"). Responses leave the queue
│   │                       only as the client grants HTTP/2 flow-control credit, so a
│   │                       reader that grants some within the limit is not ended. The
│   │                       limit runs from when the client's receive window is spent:
│   │                       until then its HTTP/2 layer accepts data whether or not the
│   │                       application reads it, so the server cannot see a reader that
│   │                       stopped. max_subscribers is the lever.
│   │                       Probe (cfg(test)) carries the after_resubscribe, before_heartbeat
│   │                       and before_sample hooks the window tests publish and release from.
│   │                       STREAM runs until the client goes, stalls or the target stops;
│   │                       ONCE ends with OK after the sync marker;
│   │                       POLL runs until the client goes, half-closes (OK once the queue
│   │                       is empty) or stalls after a poll, or the target stops. A subscription ended
│   │                       by the target stopping or going away ends UNAVAILABLE too: the
│   │                       producer sends the status on a oneshot, and Responses yields it
│   │                       after the queue drains. A publish wakes only ON_CHANGE
│   │                       subscriber tasks, and only when the value changed.
│   └── otlp_grpc.rs    ← OTLP/gRPC sink: batches Metric/LogRecord, sends via tonic gRPC
│                          unary call to OTEL Collector (feature = "otlp")
├── compiler/
│   ├── mod.rs          ← Scenario format AST types, parser, and validation:
│   │                      ScenarioFile, Defaults, Entry, AfterClause, AfterOp.
│   │                      Pre-compilation representation of scenario YAML files.
│   │                      No runtime integration — parsing only.
│   ├── parse.rs        ← YAML parser and structural validation.
│   │                      parse(), detect_version(), ParseError.
│   │                      Single-signal shorthand support. Feature-gated (config).
│   ├── normalize.rs    ← Phase 2 compilation: defaults resolution.
│   │                      normalize(), NormalizedFile, NormalizedEntry,
│   │                      NormalizeError. Flattens defaults: into every entry,
│   │                      applies built-in encoder/sink fallbacks, merges
│   │                      defaults.labels for inline entries (deferred for
│   │                      pack entries so Phase 3 can interleave pack
│   │                      shared/per-metric labels), and enforces rate
│   │                      presence. Feature-gated (config).
│   ├── expand.rs       ← Phase 3 compilation: pack expansion inside scenarios:.
│   │                      expand(), ExpandedFile, ExpandedEntry, ExpandError.
│   │                      PackResolver trait (name vs file-path classification)
│   │                      plus InMemoryPackResolver test impl. Pack entries are
│   │                      materialized into one signal per pack metric with the
│   │                      spec §2.2 five-level label precedence chain
│   │                      (defaults → shared → per-metric → entry → override)
│   │                      and entry-level after propagation. Auto-ID scheme is
│   │                      `{pack_def_name}_{entry_index}` for anonymous pack
│   │                      entries; sub-signal IDs are `{entry_id}.{metric}`
│   │                      (bare) or `{entry_id}.{metric}.{id}` for a spec that
│   │                      declares an id, which a repeated name requires. The
│   │                      earlier positional `#{spec_index}` form is retired:
│   │                      it renumbered every later spec when one was inserted
│   │                      ahead of it. Overrides are keyed by that same
│   │                      selector — a bare name against a repeated one is an
│   │                      error listing the ids, never a fan-out. Post-
│   │                      expansion uniqueness check rejects user/auto id
│   │                      collisions via ExpandError::DuplicateEntryId.
│   │                      Feature-gated (config).
│   ├── timing.rs       ← Pure threshold-crossing math per generator
│   │                      (sawtooth, step, sequence, spike, flap, saturation,
│   │                      leak, degradation, spike_event, constant). Returns
│   │                      crossing time in seconds or a typed TimingError
│   │                      (OutOfRange | Ambiguous | Unsupported). Used by the
│   │                      v2 compiler's Phase 4 `after` resolution. No
│   │                      features — compiles in `no-config` builds as well.
│   └── compile_after.rs ← Phase 4+5 compilation: after-clause resolution,
│                          dependency graph, cycle detection (Kahn + DFS),
│                          clock-group auto-assignment. compile_after(),
│                          CompiledFile, CompiledEntry, CompileAfterError.
│                          Total offset = user_phase_offset + Σ crossing_time
│                          + Σ delay; clock_group is auto-named
│                          `chain_{lowest_lex_id}` per connected component
│                          when none is explicit. Feature-gated (config).
├── verify/
│   ├── mod.rs          ← alert-expectation types for `sonda test`: ExpectConfig,
│   │                      AlertExpectation, AlertState, parse_expectations() (config feature).
│   │                      The `expect:` block is pure metadata to the compiler and runtime.
│   ├── evaluator.rs    ← pure, feature-free verdicts: Observation timelines + a deadline
│   │                      → Outcome (Pass | Late | Missed | Undecided). Owns EVERY deadline
│   │                      comparison — acquisition layers must never decide pass/fail.
│   ├── alertmanager.rs ← acquisition (feature = "http"): blocking AlertmanagerClient over
│   │                      GET /api/v2/alerts — verifies the *notification*, one hop past the
│   │                      rule evaluator. Live polling only (no range API, no time()):
│   │                      parse_alerts maps matched, unexpired alerts to Firing (never
│   │                      Pending — AM has no such state), collects their labels for the
│   │                      provenance check, and reports suppression. refine_firing_timeline
│   │                      sharpens the first firing observation with the alert's own startsAt,
│   │                      clamped between the last successful non-firing poll and the poll
│   │                      that saw it — so a skewed clock can never rewrite observed history.
│   │                      Clock offset comes from the HTTP Date header (1s granularity);
│   │                      do NOT describe these verdicts with the prometheus.rs guarantees.
│   └── prometheus.rs   ← acquisition (feature = "http"): blocking PrometheusClient with a
│                          mandatory per-request timeout. Instant queries (alert_state) drive
│                          live settling; range queries (range_timeline) reconstruct verdict
│                          timelines from the stored ALERTS samples — grid parse in
│                          parse_range_timeline, firing-series labels collected for the
│                          foreign_label_values provenance check (verify/mod.rs). Errors are
│                          SondaError::Verify, retryable by polling loops.
└── config/
    ├── mod.rs          ← BaseScheduleConfig (shared schedule/delivery fields: name, rate, duration,
    │                      gaps, bursts, cardinality_spikes, dynamic_labels, labels, sink,
    │                      phase_offset, clock_group, jitter, jitter_seed),
    │                      ScenarioConfig (embeds BaseScheduleConfig + generator + encoder, Deref/DerefMut),
    │                      LogScenarioConfig (embeds BaseScheduleConfig + generator + encoder, Deref/DerefMut),
    │                      HistogramScenarioConfig, SummaryScenarioConfig, DistributionConfig,
    │                      ScenarioEntry (Metrics|Logs|Histogram|Summary, with base() accessor),
    │                      CardinalitySpikeConfig, SpikeStrategy,
    │                      DynamicLabelConfig, DynamicLabelStrategy (Counter | ValuesList),
    │                      expand_scenario (csv_replay multi-column fan-out),
    │                      expand_log_scenario (log_csv_replay rate derivation),
    │                      expand_entry (entry-level wrapper covering both)
    ├── scalar_text.rs  ← ScalarText (config feature) + required (gnmi or snmp feature) and
    │                      optional (gnmi feature) deserialize_with: a string, number or
    │                      boolean read as text. The ONE rule for text fields the schema
    │                      widens (gnmi enum labels, gnmi_target listen and sample_floor,
    │                      snmp_agent listen, the snmp encoder's agent_label and
    │                      drop_labels items). OidText is its exception, for OID fields
    │                      (oids: values, the snmp.oid hint): a string only, read through
    │                      deserialize_any so an unquoted 1.30 is an error naming the
    │                      field rather than 1.3; their schema positions carry
    │                      x-sonda-no-widening, which schema.rs honours and strips.
    ├── aliases.rs      ← operational vocabulary aliases: desugar_entry, desugar_scenario_config.
    │                      Transforms high-level aliases (flap, steady, leak, saturation,
    │                      degradation, spike_event) into underlying GeneratorConfig variants.
    │                      Jitter-implying aliases (steady, degradation) set jitter on
    │                      BaseScheduleConfig. Integrated into prepare_entries pipeline.
    └── validate.rs     ← config validation logic, parse_duration (accepts fractional seconds via f64),
                           validate_snmp_encoder (snmp feature; from validate_config): pairing,
                           an oids: entry for the metric, placeholders are static or dynamic
                           labels, every value they take is an index_arc (counters checked at
                           the first and last value, random spikes refused), coverage, no two
                           metrics able to render one OID (snmp_collision: same arc count and
                           every position can match; a label's values are its static value or
                           values list, every u32 for a counter; O(arcs x values) per pair),
                           nothing under the system group 1.3.6.1.2.1.1; reject_snmp_for refuses the encoder
                           and the snmp_agent sink on histogram, summary and logs entries,
                           validate_cardinality_spike_config, validate_dynamic_label_config,
                           validate_histogram_config, validate_summary_config,
                           validate_distribution_config (min < max for Uniform)
```

## Cargo Features

| Feature | Default | Description |
|---------|---------|-------------|
| `config` | yes | Enables `serde::Deserialize` impls on all config types and pulls in `serde_yaml_ng` for YAML parsing. Disable for library consumers who construct configs in code and do not need YAML/JSON deserialization. |
| `strict-config` | yes | Rejects unknown YAML keys at parse time (`serde_ignored` at the deserialization choke point in `compiler/parse.rs`). Split from `config` because it compiles a second copy of the whole `ScenarioFile` deserializer: +170 KB on the optimized wasm bundle. The binaries enable it via their own `config` feature; `sonda-wasm` deliberately does not, so the playground parses as it did before. |
| `runtime` | yes | The async scheduling and delivery layer: `tokio`, `tokio-util`, the `schedule/` module, `emit.rs`, the tokio-backed sinks (`stdout`, `file`, `tcp`, `udp`, `channel`), and `create_sink()`. Disable for pure-engine consumers — generators, encoders, the compiler, and config types all work without it, which is how `sonda-wasm` compiles the engine to `wasm32-unknown-unknown`. The `http`/`kafka`/`remote-write`/`otlp`/`gnmi`/`snmp` features all imply `runtime`. |
| `http` | no | Enables `ureq` and HTTP-based sinks (`HttpPush`, `Loki`). |
| `kafka` | no | Enables `rskafka` + `tokio` + `rustls` + `rustls-pemfile` + `webpki-roots` for the Kafka sink with TLS and SASL support. |
| `remote-write` | no | Enables `prost` + `snap` + `ureq` for the Prometheus remote write encoder and sink. |
| `otlp` | no | Enables `tonic` + `prost` + `tokio` + `bytes` + `http` for the OTLP encoder and gRPC sink. |
| `snmp` | no | Enables `bytes` and `runtime` for the SNMP encoder (`encoder/snmp/`): its hand-written BER codec, OID templates and publish records. `SnmpValueType` and the `snmp:` pack hint compile without it. Validation pairs the encoder with the `snmp_agent` sink only, in both directions. |
| `gnmi` | no | Enables `prost`, `tonic`, `bytes`, `http`, `tower`, `tokio-stream`, `rustls` and `rustls-pki-types` (both already built by tonic's TLS: the ring provider installed as the process default when none is, and the `tls` PEM files parsed) and `runtime` for the gNMI encoder (`encoder/gnmi/`) and the `gnmi_target` sink (`sink/gnmi/`). Validation pairs the encoder with the `gnmi_target` sink only, in both directions. |

When the `config` feature is disabled:
- All config types (`ScenarioConfig`, `EncoderConfig`, `SinkConfig`, `GeneratorConfig`, etc.) remain
  public and constructible in code.
- `Deserialize` impls and `#[serde(...)]` attributes are conditionally compiled out.
- `serde_yaml_ng` is not linked. `serde_json` remains available (used by the JSON encoder).
- Tests that parse YAML are gated behind `#[cfg(feature = "config")]`.

## How to Add a New Generator

1. Create `src/generator/your_name.rs` with a struct that implements `ValueGenerator`.
2. The struct must be `Send + Sync`. Store configuration in the struct fields.
3. Implement `fn value(&self, tick: u64) -> f64`. This is a pure function — no side effects.
4. Register it in `src/generator/mod.rs`:
   - Add a variant to the `GeneratorConfig` enum (serde-tagged).
   - Add a match arm in `create_generator()` that returns `Box::new(YourGenerator::new(...))`.
5. Add unit tests in the same file: test determinism, edge cases, boundary values.
6. Update the YAML config schema doc if it exists.

## How to Add a New Encoder

Same pattern as generators:
1. Create `src/encoder/your_format.rs` implementing the `Encoder` trait.
2. The `encode_metric` method writes into a caller-provided `&mut Vec<u8>`. Never allocate a new
   buffer — reuse what is given.
3. Pre-build any invariant content (label prefix, metric name validation) in `new()`. The encode
   method should do as little work as possible per event.
4. Register in `src/encoder/mod.rs` factory.
5. Test with known inputs → expected byte output. Use `assert_eq!(String::from_utf8(buf).unwrap(), ...)`.

### Encoder Precision

The `PrometheusText`, `InfluxLineProtocol`, and `JsonLines` encoders support an optional
`precision: Option<u8>` field that limits decimal places in formatted metric values. When `None`,
full `f64` precision is preserved (default behavior). When set, values are formatted with the
specified number of decimal places. Use `write_value()` from `encoder/mod.rs` for text encoders;
JSON encoders pre-round the value before passing it to serde. Precision is validated in
`config/validate.rs` (must be 0..=17).

## How to Add a New Sink

1. Create `src/sink/your_sink.rs` implementing the `Sink` trait.
2. `write(&mut self, data: &[u8])` delivers one encoded event. `flush(&mut self)` forces delivery.
3. Sinks own their I/O resources (file handles, sockets, etc.).
4. Register in `src/sink/mod.rs` factory. The `create_sink()` function accepts an optional
   `labels: Option<&HashMap<String, String>>` parameter. Most sinks ignore this (pass `None`).
   The Loki sink uses it for stream labels, sourced from the scenario-level `labels` config.
5. **Override `write_log_event` when the sink consumes per-event context.** The default impl
   discards the `&LogEvent` and forwards the encoded bytes to `write()`. If your sink uses any
   per-event field (labels, severity, timestamp) for delivery routing — stream selection,
   partition keys, structured envelopes — you MUST override `write_log_event`. Inheriting the
   default silently drops that context at runtime and produces wrong output (see `sink/loki.rs`
   for the in-tree example: it overrides to promote `LogEvent.labels` into the Loki stream
   label set).
6. Test with a mock or in-memory buffer sink.

## Performance Guidelines

- **No per-event allocations.** The hot path is: generate value → build MetricEvent → encode into
  buffer → write to sink. Each step should write into pre-allocated or caller-provided memory.
- **Arc-wrapped MetricEvent fields.** `MetricEvent::name` is a `ValidatedMetricName` (newtype
  over `Arc<str>` that validates the metric name regex once at construction) and `MetricEvent::labels`
  is `Arc<Labels>`. Cloning a MetricEvent is O(1) — just reference-count bumps. The metric runner
  constructs a `ValidatedMetricName` once before the loop and uses `MetricEvent::from_parts` (no
  per-tick validation) with `name.clone()` (no per-tick heap allocation). Only when a cardinality
  spike is active does the runner deep-clone the inner Labels to insert the spike key.
- **Zero-alloc timestamp formatting.** `format_rfc3339_millis_array` writes the RFC 3339 timestamp
  into a stack-allocated `[u8; 24]` — no heap allocation. JSON and syslog encoders use this to
  borrow a `&str` without going through `String`. The `format_rfc3339_millis(ts, buf)` variant
  appends directly into the caller's `Vec<u8>` buffer.
- **Zero-allocation JSON label serialization.** The JSON encoder serializes labels and fields
  directly from their source iterators via `LabelsRef` and `StringMapRef` wrappers that implement
  `serde::Serialize`. No intermediate `BTreeMap<&str, &str>` is collected per event. The source
  `Labels` (BTreeMap) and `fields` (BTreeMap) already iterate in sorted order.
- **Single-pass log template resolution.** `LogTemplateGenerator::resolve_template` scans the
  template string once, writing literal segments and resolved `{placeholder}` values directly into
  a pre-allocated output buffer. This replaces the previous N-pass `String::replace` approach that
  allocated a new `String` per placeholder.
- **Pre-build label strings.** Labels don't change between events for a given scenario. Build the
  serialized label prefix once at construction time.
- **Use `BufWriter`.** Never write individual lines to stdout or files without buffering.
- **Benchmark before optimizing.** Use `cargo bench` with criterion if you suspect a bottleneck.
  Don't optimize speculatively.

## Error Handling

- Define errors in `src/lib.rs` using `thiserror`.
- Every public function returns `Result<T, SondaError>`.
- Never `unwrap()` in this crate. Use `?` propagation or explicit error mapping.
- **Structured error sub-enums**: `SondaError` delegates to typed sub-enums that preserve
  original error sources via `#[source]`:
  - `ConfigError` — configuration validation errors. Use `ConfigError::invalid(msg)` to construct.
  - `GeneratorError` — generator I/O errors. `FileRead { path, source: io::Error }` preserves
    the original I/O error for programmatic inspection (e.g., `ErrorKind::NotFound`).
  - `EncoderError` — encoding errors. `SerializationFailed(serde_json::Error)` and
    `TimestampBeforeEpoch(SystemTimeError)` preserve the original error. `NotSupported(String)`
    for unsupported event types. `EventRejected(String)` when one event does not fit a valid
    encoder configuration (the gnmi encoder: missing or uncovered label, unencodable value,
    unmapped enum code). The schedule loop skips such an event, counts it in
    `ScenarioStats::rejected_events` and `errors`, and keeps running. `Other(String)` for feature-gated encoder errors (protobuf, snappy).
  - `RuntimeError` — system/environment errors. `SpawnFailed(#[source] io::Error)` for thread
    spawn failures (preserves the original `io::Error` via `#[source]`), `ThreadPanicked` for
    panicked scenario threads, `ScenariosFailed(String)` for collected errors from multi-scenario
    thread joins. Separated from `ConfigError` so consumers matching on config errors are not
    confused by system failures.
- `SondaError::Sink` wraps `std::io::Error` **without** a blanket `#[from]` conversion.
  All I/O errors must be explicitly mapped to the correct variant at each call site:
  - Sink I/O errors: use `.map_err(SondaError::Sink)` or `SondaError::Sink(io_err)`.
  - Generator file I/O errors: use `SondaError::Generator(GeneratorError::FileRead { path, source })`.
  - Config validation errors: use `SondaError::Config(ConfigError::invalid(msg))`.
  - Runtime errors: use `SondaError::Runtime(RuntimeError::SpawnFailed(io_err))`.
  - Multi-scenario thread failures: use `SondaError::Runtime(RuntimeError::ScenariosFailed(msg))`.
- This prevents generator or config I/O errors from being misclassified as sink errors.

## Testing

### Unit tests (embedded in source files)

- Every generator: test at tick=0, tick=1, tick at period boundary, tick at large values.
- Every encoder: test with known MetricEvent → assert exact byte output.
- Schedule logic: test gap window math, burst window transitions, rate calculation.
- Use `#[cfg(test)] mod tests` at the bottom of each file.
- Seed all RNG-based generators for deterministic tests.

### Parametrized tests — use `rstest`

For test families that share a shape and differ only by input/expected output, use `rstest`:

- Each `#[case::<descriptive_name>(...)]` row reports as a distinct test in `cargo test` output
  — failure location is preserved per case. Use descriptive labels, not anonymous cases.
- Apply `#[rustfmt::skip]` on the rstest fn so `#[case(...)]` rows stay column-aligned.
- Do NOT force-fit singleton or semantically-unique cases into tables just to reduce line count.
  A standalone `#[test]` fn is better than a table with heterogeneous payloads.
- Example patterns: `compiler::timing::tests` (crossing math across generator families),
  `compiler::parse::tests` (invalid-YAML rejection variants).

### Integration tests (`sonda-core/tests/*.rs`)

- Start every file with `mod common;` to pull in shared helpers from `tests/common/mod.rs`:
  `example_fixture`, `parity_fixture`, `load_repo_pack`, `builtin_pack_resolver`, `resolver_with`,
  `compile_to_expanded`, `compile_to_compiled`, `snapshot_settings`. `common` is compiled only
  with `config`; `tests/common/alloc.rs` (the per-thread `CountingAlloc` an allocation-budget
  test installs as its `#[global_allocator]`, with `count_allocations`) is included with
  `#[path = "common/alloc.rs"] mod counting;` so it builds without `config` too.
- Add new helpers to `tests/common/mod.rs` — do not duplicate across test files.

### Golden-file snapshots — use `insta`

- Snapshots live under `sonda-core/tests/snapshots/*.snap` and are updated via
  `cargo insta review` (interactive) or `INSTA_UPDATE=always cargo test` (batch).
- The hand-rolled harness previously at `src/config/snapshot.rs` was replaced by insta — do not
  reintroduce it.
- Structured outputs (compiled/expanded/normalized): wrap `insta::assert_json_snapshot!(value, ...)`
  in `common::snapshot_settings().bind(|| ...)` for the shared `sort_maps = true` determinism config.
- Text outputs (stdout bytes, log lines): `insta::assert_snapshot!(text)`.
- Parametrized families: combine rstest + insta — each `#[case(...)]` generates its own snapshot
  name automatically.
