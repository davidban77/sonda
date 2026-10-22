//! Which series a gNMI `SubscriptionList` or `GetRequest` selects.

use tonic::Status;

use crate::encoder::gnmi::proto::{self, subscription_list, Encoding, SubscriptionMode};

/// The requested paths of one `SubscriptionList` or `GetRequest`, each
/// joined to the request's prefix.
///
/// A requested path is the prefix's elements followed by the path's own,
/// with the path's origin (the prefix's when the path has none) and the
/// prefix's target. An element named `*` stands for any one element and
/// `...` for any run of zero or more.
#[derive(Debug, Clone)]
pub struct SubscriptionMatcher {
    paths: Vec<proto::Path>,
}

impl SubscriptionMatcher {
    /// Join each subscription in `list` to the list's prefix.
    ///
    /// # Errors
    ///
    /// - `INVALID_ARGUMENT` when the list has no subscriptions, a path is
    ///   refused as [`SubscriptionMatcher::from_paths`] refuses it, or the
    ///   list's encoding or mode, or in a `STREAM` list a subscription's
    ///   mode, is a number gNMI does not define. The subscription modes of a
    ///   `ONCE` or `POLL` list are not read.
    /// - `UNIMPLEMENTED` as [`served_encoding`] refuses the encoding.
    pub fn new(list: &proto::SubscriptionList) -> Result<Self, Status> {
        served_encoding(list.encoding)?;
        let mode = subscription_list::Mode::try_from(list.mode).map_err(|_| {
            Status::invalid_argument(format!("unknown subscription list mode {}", list.mode))
        })?;
        if list.subscription.is_empty() {
            return Err(Status::invalid_argument(
                "the subscription list has no subscriptions",
            ));
        }
        if mode == subscription_list::Mode::Stream {
            for subscription in &list.subscription {
                SubscriptionMode::try_from(subscription.mode).map_err(|_| {
                    Status::invalid_argument(format!(
                        "unknown subscription mode {}",
                        subscription.mode
                    ))
                })?;
            }
        }
        let root = proto::Path::default();
        Self::join(
            list.prefix.as_ref(),
            list.subscription
                .iter()
                .map(|subscription| subscription.path.as_ref().unwrap_or(&root)),
        )
    }

    /// Join each of `paths` to `prefix`, as a `GetRequest` asks.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` when `paths` is empty, or a joined path has an
    /// element with an empty name or a `...` element with keys.
    pub fn from_paths(prefix: Option<&proto::Path>, paths: &[proto::Path]) -> Result<Self, Status> {
        if paths.is_empty() {
            return Err(Status::invalid_argument("the request has no paths"));
        }
        Self::join(prefix, paths.iter())
    }

    /// Join each of `paths` to `prefix`, refusing an element with an empty
    /// name and a `...` element with keys: the one rule [`Self::new`] and
    /// [`Self::from_paths`] apply.
    fn join<'a>(
        prefix: Option<&proto::Path>,
        paths: impl Iterator<Item = &'a proto::Path>,
    ) -> Result<Self, Status> {
        let root = proto::Path::default();
        let prefix = prefix.unwrap_or(&root);
        let mut joined = Vec::new();
        for own in paths {
            let mut elem = Vec::with_capacity(prefix.elem.len() + own.elem.len());
            elem.extend(prefix.elem.iter().cloned());
            elem.extend(own.elem.iter().cloned());
            for element in &elem {
                if element.name.is_empty() {
                    return Err(Status::invalid_argument(
                        "a requested path has an element with an empty name",
                    ));
                }
                if element.name == ANY_RUN && !element.key.is_empty() {
                    return Err(Status::invalid_argument(
                        "a requested path has a `...` element with keys; `...` stands \
                         for any run of elements and takes no keys",
                    ));
                }
            }
            let origin = if own.origin.is_empty() {
                &prefix.origin
            } else {
                &own.origin
            };
            joined.push(proto::Path {
                origin: origin.clone(),
                elem,
                target: prefix.target.clone(),
            });
        }
        Ok(Self { paths: joined })
    }

    /// The requested paths, each joined to the prefix, in request order.
    pub fn paths(&self) -> &[proto::Path] {
        &self.paths
    }

    /// True if `series_path` is any subscription path or below one.
    ///
    /// The subscription's elements are aligned with the series' first
    /// elements: a named element with an element of that name, `*` with any
    /// one element, `...` with any run of zero or more. A subscription key of
    /// `*` or an absent key matches any value; a key on a named element or on
    /// `*` must otherwise equal the series element's. An empty subscription
    /// origin or target matches any; otherwise it must equal the series'.
    /// Allocates nothing.
    pub fn matches(&self, series_path: &proto::Path) -> bool {
        self.paths.iter().any(|path| is_under(series_path, path))
    }
}

/// The encoding a request carrying `encoding` is served in: `PROTO` for
/// `PROTO` and for `JSON`, which is encoding 0 and so also what a request
/// that sets no encoding carries; `JSON_IETF` for `JSON_IETF`.
///
/// # Errors
///
/// - `INVALID_ARGUMENT` when `encoding` is a number gNMI does not define.
/// - `UNIMPLEMENTED`, naming the encoding and the ones served, for `BYTES`
///   and `ASCII`.
pub(crate) fn served_encoding(encoding: i32) -> Result<Encoding, Status> {
    match Encoding::try_from(encoding) {
        Ok(Encoding::Proto | Encoding::Json) => Ok(Encoding::Proto),
        Ok(Encoding::JsonIetf) => Ok(Encoding::JsonIetf),
        Ok(other) => Err(Status::unimplemented(format!(
            "encoding {} is not supported; this target encodes PROTO and JSON_IETF",
            encoding_name(other)
        ))),
        Err(_) => Err(Status::invalid_argument(format!(
            "unknown gnmi encoding {encoding}"
        ))),
    }
}

/// The name `gnmi.proto` gives `encoding`.
fn encoding_name(encoding: Encoding) -> &'static str {
    match encoding {
        Encoding::Json => "JSON",
        Encoding::Bytes => "BYTES",
        Encoding::Proto => "PROTO",
        Encoding::Ascii => "ASCII",
        Encoding::JsonIetf => "JSON_IETF",
    }
}

/// The element name that stands for any one element.
const ANY_ONE: &str = "*";

/// The element name that stands for any run of zero or more elements.
const ANY_RUN: &str = "...";

/// Whether `series` is `subscribed` or below it.
fn is_under(series: &proto::Path, subscribed: &proto::Path) -> bool {
    (subscribed.origin.is_empty() || subscribed.origin == series.origin)
        && (subscribed.target.is_empty() || subscribed.target == series.target)
        && aligns(&subscribed.elem, &series.elem)
}

/// Whether `wanted` aligns with the first elements of `series`, as
/// [`SubscriptionMatcher::matches`] describes. Each `...` first takes no
/// elements; on a mismatch the latest `...` takes one more and the elements
/// after it are aligned again, which finds an alignment whenever one exists.
fn aligns(wanted: &[proto::PathElem], series: &[proto::PathElem]) -> bool {
    let (mut w, mut s) = (0, 0);
    // The latest `...`, and the series position its run ends at.
    let mut run: Option<(usize, usize)> = None;
    loop {
        let Some(element) = wanted.get(w) else {
            return true;
        };
        if element.name == ANY_RUN {
            run = Some((w, s));
            w += 1;
        } else if series.get(s).is_some_and(|e| element_matches(element, e)) {
            w += 1;
            s += 1;
        } else {
            match run {
                Some((at, end)) if end < series.len() => {
                    run = Some((at, end + 1));
                    w = at + 1;
                    s = end + 1;
                }
                _ => return false,
            }
        }
    }
}

/// Whether the subscription element `wanted`, a name or `*`, matches the
/// series element `elem`: the same name unless `*`, and each of its keys `*`
/// or equal to `elem`'s.
fn element_matches(wanted: &proto::PathElem, elem: &proto::PathElem) -> bool {
    (wanted.name == ANY_ONE || wanted.name == elem.name)
        && wanted
            .key
            .iter()
            .all(|(k, v)| v == "*" || elem.key.get(k) == Some(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::gnmi::path::parse_client_path;

    fn path(s: &str) -> proto::Path {
        parse_client_path(s).expect("a valid path")
    }

    /// A STREAM/SAMPLE list in PROTO with one subscription per path.
    fn list(paths: &[&str]) -> proto::SubscriptionList {
        proto::SubscriptionList {
            prefix: None,
            subscription: paths
                .iter()
                .map(|p| proto::Subscription {
                    path: Some(path(p)),
                    mode: SubscriptionMode::Sample as i32,
                    ..Default::default()
                })
                .collect(),
            mode: subscription_list::Mode::Stream as i32,
            encoding: Encoding::Proto as i32,
            updates_only: false,
        }
    }

    fn matcher(paths: &[&str]) -> SubscriptionMatcher {
        SubscriptionMatcher::new(&list(paths)).expect("a supported list")
    }

    #[test]
    fn wildcard_key_matches_any_interface() {
        let m = matcher(&["/interfaces/interface[name=*]/state/counters"]);
        let eth0 = path("/interfaces/interface[name=eth0]/state/counters/in-octets");
        let eth1 = path("/interfaces/interface[name=eth1]/state/counters/out-octets");
        assert!(m.matches(&eth0));
        assert!(m.matches(&eth1));
        assert!(
            !m.matches(&path("/interfaces/interface[name=eth0]/state/oper-status")),
            "a key wildcard still needs the remaining elements to match"
        );

        let absent = matcher(&["/interfaces/interface/state/counters"]);
        assert!(absent.matches(&eth0), "an absent key matches any value");
        assert!(absent.matches(&eth1));

        let exact = matcher(&["/interfaces/interface[name=eth0]/state/counters"]);
        assert!(exact.matches(&eth0));
        assert!(
            !exact.matches(&eth1),
            "a literal key matches only its value"
        );
    }

    #[test]
    fn prefix_match_excludes_siblings() {
        let m = matcher(&["/interfaces/interface[name=eth0]/state/counters"]);
        assert!(m.matches(&path(
            "/interfaces/interface[name=eth0]/state/counters/in-octets"
        )));
        assert!(
            m.matches(&path("/interfaces/interface[name=eth0]/state/counters")),
            "the subscribed path itself is under it"
        );
        for sibling in [
            "/interfaces/interface[name=eth0]/state/oper-status",
            "/interfaces/interface[name=eth0]/config/counters/in-octets",
            "/interfaces/interface[name=eth0]/state",
            "/system/state/counters/in-octets",
        ] {
            assert!(!m.matches(&path(sibling)), "{sibling} is not under it");
        }
    }

    #[test]
    fn the_list_prefix_is_joined_to_each_path() {
        let mut list = list(&["/state/counters"]);
        list.prefix = Some(path("openconfig:/interfaces/interface[name=eth0]"));
        let m = SubscriptionMatcher::new(&list).expect("a supported list");
        assert!(m.matches(&path(
            "openconfig:/interfaces/interface[name=eth0]/state/counters/in-octets"
        )));
        assert!(!m.matches(&path("openconfig:/state/counters/in-octets")));
        assert!(
            !m.matches(&path(
                "other:/interfaces/interface[name=eth0]/state/counters/in-octets"
            )),
            "the prefix's origin applies"
        );
    }

    #[test]
    fn origin_and_target_match_when_set() {
        let series = proto::Path {
            target: "rtr-1".to_string(),
            ..path("openconfig:/interfaces/interface[name=eth0]/state")
        };
        assert!(
            matcher(&["/interfaces"]).matches(&series),
            "no origin matches any"
        );
        assert!(matcher(&["openconfig:/interfaces"]).matches(&series));
        assert!(!matcher(&["vendor:/interfaces"]).matches(&series));

        let mut list = list(&["/interfaces"]);
        list.prefix = Some(proto::Path {
            target: "rtr-2".to_string(),
            ..Default::default()
        });
        assert!(!SubscriptionMatcher::new(&list).unwrap().matches(&series));
        list.prefix = Some(proto::Path {
            target: "rtr-1".to_string(),
            ..Default::default()
        });
        assert!(SubscriptionMatcher::new(&list).unwrap().matches(&series));
    }

    #[test]
    fn any_subscription_in_the_list_matches() {
        let m = matcher(&["/system", "/interfaces/interface[name=eth1]"]);
        assert!(m.matches(&path("/system/state/hostname")));
        assert!(m.matches(&path("/interfaces/interface[name=eth1]/state")));
        assert!(!m.matches(&path("/interfaces/interface[name=eth0]/state")));
    }

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::unknown_list_mode(9, SubscriptionMode::Sample as i32, Encoding::Proto as i32, tonic::Code::InvalidArgument, "mode 9")]
    #[case::unknown_mode(subscription_list::Mode::Stream as i32, 9, Encoding::Proto as i32, tonic::Code::InvalidArgument, "mode 9")]
    #[case::unknown_encoding(subscription_list::Mode::Stream as i32, SubscriptionMode::Sample as i32, 9, tonic::Code::InvalidArgument, "encoding 9")]
    fn unsupported_requests_are_refused_by_name(
        #[case] list_mode: i32,
        #[case] mode: i32,
        #[case] encoding: i32,
        #[case] code: tonic::Code,
        #[case] named: &str,
    ) {
        let mut list = list(&["/interfaces"]);
        list.mode = list_mode;
        list.encoding = encoding;
        list.subscription[0].mode = mode;
        let status = SubscriptionMatcher::new(&list).expect_err("refused");
        assert_eq!(status.code(), code, "{}", status.message());
        assert!(status.message().contains(named), "{}", status.message());
    }

    /// An encoding this target does not serve is refused, naming it and
    /// the ones served.
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::bytes(Encoding::Bytes, "BYTES")]
    #[case::ascii(Encoding::Ascii, "ASCII")]
    fn refused_encodings_name_the_request_and_the_served(
        #[case] encoding: Encoding,
        #[case] named: &str,
    ) {
        let mut list = list(&["/interfaces"]);
        list.encoding = encoding as i32;
        let status = SubscriptionMatcher::new(&list).expect_err("refused");
        assert_eq!(status.code(), tonic::Code::Unimplemented, "{}", status.message());
        assert!(status.message().contains(named), "{}", status.message());
        assert!(status.message().contains("PROTO and JSON_IETF"), "{}", status.message());
    }

    /// PROTO and JSON_IETF are accepted, and so is JSON: encoding 0, which a
    /// list that sets no encoding also carries.
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::proto(Encoding::Proto)]
    #[case::json_ietf(Encoding::JsonIetf)]
    #[case::json_or_unset(Encoding::Json)]
    fn served_encodings_are_accepted(#[case] encoding: Encoding) {
        let mut list = list(&["/interfaces"]);
        list.encoding = encoding as i32;
        assert!(SubscriptionMatcher::new(&list).is_ok());
    }

    /// The modes this target serves are accepted, and in a ONCE or POLL
    /// list the per-subscription mode does not apply.
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::stream_sample(subscription_list::Mode::Stream, SubscriptionMode::Sample)]
    #[case::stream_on_change(subscription_list::Mode::Stream, SubscriptionMode::OnChange)]
    #[case::stream_target_defined(subscription_list::Mode::Stream, SubscriptionMode::TargetDefined)]
    #[case::once(subscription_list::Mode::Once, SubscriptionMode::OnChange)]
    #[case::poll(subscription_list::Mode::Poll, SubscriptionMode::OnChange)]
    fn supported_modes_are_accepted(
        #[case] list_mode: subscription_list::Mode,
        #[case] mode: SubscriptionMode,
    ) {
        let mut list = list(&["/interfaces"]);
        list.mode = list_mode as i32;
        list.subscription[0].mode = mode as i32;
        assert!(SubscriptionMatcher::new(&list).is_ok());
    }

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::element_wildcard("/interfaces/*/state", "/interfaces/interface[name=eth0]/state/oper-status")]
    #[case::multi_level_wildcard("/interfaces/.../state", "/interfaces/interface[name=eth0]/state/oper-status")]
    fn element_wildcards_are_accepted(#[case] p: &str, #[case] series: &str) {
        let m = SubscriptionMatcher::new(&list(&[p])).expect("accepted");
        assert!(m.matches(&path(series)), "{p} matches {series}");
    }

    /// Every path `examples/gnmi-telegraf.yaml` renders, counters and
    /// oper-status, with its metric's name: each metrics entry rendered
    /// through its encoder, with the origin and the target label's value on
    /// the path.
    #[cfg(feature = "config")]
    fn gnmi_telegraf_example_paths() -> Vec<(String, proto::Path)> {
        use crate::compiler::expand::InMemoryPackResolver;
        use crate::config::ScenarioEntry;
        use crate::encoder::gnmi::GnmiEncoder;
        use crate::encoder::EncoderConfig;
        use crate::model::metric::Labels;

        let file = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../examples/gnmi-telegraf.yaml"
        );
        let yaml = std::fs::read_to_string(file).expect("examples/gnmi-telegraf.yaml");
        let entries = crate::compile_scenario_file(&yaml, &InMemoryPackResolver::new())
            .expect("the example compiles");
        entries
            .iter()
            .map(|entry| {
                let ScenarioEntry::Metrics(config) = entry else {
                    panic!("the example holds metrics entries only");
                };
                let EncoderConfig::Gnmi(cfg) = &config.encoder else {
                    panic!("the example encodes gnmi");
                };
                let static_labels = config.labels.clone().unwrap_or_default();
                let pairs: Vec<(&str, &str)> = static_labels
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                let labels = Labels::from_pairs(&pairs).expect("valid labels");
                let encoder = GnmiEncoder::new(cfg).expect("a valid encoder");
                let mut rendered = encoder
                    .template_for(&config.name)
                    .and_then(|t| t.render(&config.name, &labels))
                    .expect("the path renders");
                rendered.origin.clone_from(&cfg.origin);
                rendered.target = static_labels[&cfg.target_label].clone();
                (config.name.clone(), rendered)
            })
            .collect()
    }

    /// `/interfaces/.../in-octets` matches the `in-octets` path
    /// `examples/gnmi-telegraf.yaml` renders on every interface and nothing
    /// else; `/interfaces/.../counters` matches its counter paths and not its
    /// oper-status paths.
    #[cfg(feature = "config")]
    #[test]
    fn a_run_wildcard_matches_the_gnmi_telegraf_example_counters() {
        let paths = gnmi_telegraf_example_paths();
        let names: std::collections::BTreeSet<&str> =
            paths.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            ["in_octets", "oper_status", "out_octets"].into(),
            "the premise: the example's metrics"
        );
        assert_eq!(
            paths.len(),
            12,
            "the premise: four interfaces, three metrics each"
        );

        let in_octets = matcher(&["/interfaces/.../in-octets"]);
        let counters = matcher(&["/interfaces/.../counters"]);
        for (name, series) in &paths {
            assert_eq!(
                in_octets.matches(series),
                name == "in_octets",
                "{name}: {series}"
            );
            assert_eq!(
                counters.matches(series),
                name != "oper_status",
                "{name}: {series}"
            );
        }
    }

    /// `*` stands for exactly one element of any name: not none, not two.
    #[test]
    fn an_element_wildcard_stands_for_exactly_one_element() {
        let m = matcher(&["/interfaces/*/state/counters"]);
        assert!(m.matches(&path(
            "/interfaces/interface[name=eth0]/state/counters/in-octets"
        )));
        assert!(m.matches(&path("/interfaces/subinterface/state/counters")));
        assert!(
            !m.matches(&path("/interfaces/interface/subinterface/state/counters")),
            "two elements where `*` stands for one"
        );
        assert!(
            !m.matches(&path("/interfaces/state/counters")),
            "no element where `*` stands for one"
        );
    }

    /// `...` stands for any run of elements, none included; a named element
    /// after it must still appear, with its keys.
    #[test]
    fn a_run_wildcard_stands_for_zero_or_more_elements() {
        let eth0 = path("/interfaces/interface[name=eth0]/state/counters/in-octets");
        let eth1 = path("/interfaces/interface[name=eth1]/state/counters/in-octets");
        assert!(
            matcher(&["/interfaces/.../interface/state"]).matches(&eth0),
            "`...` takes no elements"
        );
        assert!(matcher(&["/interfaces/.../counters"]).matches(&eth0));
        assert!(
            matcher(&["/..."]).matches(&eth0),
            "`...` alone matches everything"
        );
        assert!(
            matcher(&["/interfaces/..."]).matches(&path("/interfaces")),
            "a trailing `...` takes no elements"
        );
        assert!(
            !matcher(&["/interfaces/.../config"]).matches(&eth0),
            "a named element after `...` that never appears"
        );
        assert!(
            !matcher(&["/system/..."]).matches(&eth0),
            "the elements before `...` still apply"
        );

        let keyed = matcher(&["/interfaces/.../interface[name=eth0]/state"]);
        assert!(keyed.matches(&eth0));
        assert!(!keyed.matches(&eth1), "keys after `...` still apply");
        let keyed_any = matcher(&["/interfaces/*[name=eth0]/state"]);
        assert!(keyed_any.matches(&eth0));
        assert!(!keyed_any.matches(&eth1), "keys on `*` still apply");
    }

    /// An alignment that needs `...` to take more than its first try: the
    /// first `b` is followed by `x`, not `c`.
    #[test]
    fn a_run_wildcard_takes_more_elements_when_the_first_try_fails() {
        assert!(matcher(&["/a/.../b/c"]).matches(&path("/a/b/x/b/c/d")));
        assert!(matcher(&["/.../b/.../d"]).matches(&path("/a/b/c/d")));
        assert!(!matcher(&["/.../b/.../d"]).matches(&path("/a/c/d")));
    }

    /// Whether `wanted` aligns with the first elements of `series`, by the
    /// definition: an empty subscription aligns; `...` aligns when the rest
    /// aligns after it takes any number of elements; any other element
    /// aligns when it matches the series' first element and the rest aligns
    /// after it.
    fn aligns_by_definition(wanted: &[proto::PathElem], series: &[proto::PathElem]) -> bool {
        match wanted.split_first() {
            None => true,
            Some((first, rest)) if first.name == ANY_RUN => {
                (0..=series.len()).any(|taken| aligns_by_definition(rest, &series[taken..]))
            }
            Some((first, rest)) => series.split_first().is_some_and(|(elem, after)| {
                element_matches(first, elem) && aligns_by_definition(rest, after)
            }),
        }
    }

    /// Every element sequence of length up to `max` over `names`.
    fn sequences(names: &[&str], max: usize) -> Vec<Vec<proto::PathElem>> {
        let mut all = vec![Vec::new()];
        let mut last = vec![Vec::new()];
        for _ in 0..max {
            last = last
                .iter()
                .flat_map(|prefix: &Vec<proto::PathElem>| {
                    names.iter().map(move |name| {
                        let mut next = prefix.clone();
                        next.push(proto::PathElem {
                            name: name.to_string(),
                            key: Default::default(),
                        });
                        next
                    })
                })
                .collect();
            all.extend(last.iter().cloned());
        }
        all
    }

    /// The alignment agrees with its definition for every subscription of up
    /// to five elements over `a`, `b`, `*` and `...` and every series of up
    /// to six elements over `a` and `b`.
    #[test]
    fn the_alignment_agrees_with_its_definition() {
        let wanted = sequences(&["a", "b", ANY_ONE, ANY_RUN], 5);
        let series = sequences(&["a", "b"], 6);
        assert_eq!(
            (wanted.len(), series.len()),
            (1365, 127),
            "the premise: the inputs"
        );
        let mut matched = 0usize;
        for w in &wanted {
            for s in &series {
                let expected = aligns_by_definition(w, s);
                assert_eq!(aligns(w, s), expected, "{w:?} against {s:?}");
                matched += usize::from(expected);
            }
        }
        assert!(
            matched > 0 && matched < wanted.len() * series.len(),
            "the premise: both outcomes occur ({matched})"
        );
    }

    /// A `...` element with keys is refused: it stands for a run of elements.
    #[test]
    fn a_run_wildcard_with_keys_is_an_invalid_argument() {
        let status = SubscriptionMatcher::new(&list(&["/interfaces/...[name=eth0]/state"]))
            .expect_err("refused");
        assert_eq!(
            status.code(),
            tonic::Code::InvalidArgument,
            "{}",
            status.message()
        );
        assert!(status.message().contains("`...`"), "{}", status.message());
    }

    /// `from_paths` joins each path to the prefix as `new` joins each
    /// subscription: the prefix's elements first, the path's origin over the
    /// prefix's, the prefix's target.
    #[test]
    fn from_paths_joins_each_path_to_the_prefix() {
        let prefix = proto::Path {
            target: "rtr-1".to_string(),
            ..path("openconfig:/interfaces/interface[name=eth0]")
        };
        let m = SubscriptionMatcher::from_paths(
            Some(&prefix),
            &[path("/state/counters"), path("vendor:/state/oper-status")],
        )
        .expect("accepted");
        let joined: Vec<String> = m.paths().iter().map(ToString::to_string).collect();
        assert_eq!(
            joined,
            [
                "openconfig:/interfaces/interface[name=eth0]/state/counters",
                "vendor:/interfaces/interface[name=eth0]/state/oper-status",
            ]
        );
        assert!(m.paths().iter().all(|p| p.target == "rtr-1"));

        let mut list = list(&["/state/counters", "vendor:/state/oper-status"]);
        list.prefix = Some(prefix);
        let subscribed = SubscriptionMatcher::new(&list).expect("accepted");
        let series = [
            "openconfig:/interfaces/interface[name=eth0]/state/counters/in-octets",
            "vendor:/interfaces/interface[name=eth0]/state/oper-status",
            "openconfig:/interfaces/interface[name=eth0]/state/oper-status",
            "openconfig:/interfaces/interface[name=eth1]/state/counters/in-octets",
        ];
        let mut matched = 0;
        for s in series {
            let s = proto::Path {
                target: "rtr-1".to_string(),
                ..path(s)
            };
            assert_eq!(m.matches(&s), subscribed.matches(&s), "{s}");
            matched += usize::from(m.matches(&s));
        }
        assert_eq!(matched, 2, "the premise: both outcomes occur");
    }

    /// `from_paths` and `new` refuse the same paths with the same status,
    /// and `from_paths` refuses a request with no paths.
    #[test]
    fn from_paths_refuses_what_new_refuses() {
        let empty_name = proto::Path {
            elem: vec![proto::PathElem::default()],
            ..Default::default()
        };
        let run_with_keys = path("/interfaces/...[name=eth0]/state");
        for bad in [empty_name, run_with_keys] {
            let from_paths = SubscriptionMatcher::from_paths(None, std::slice::from_ref(&bad))
                .expect_err("refused");
            let mut list = list(&["/x"]);
            list.subscription[0].path = Some(bad.clone());
            let new = SubscriptionMatcher::new(&list).expect_err("refused");
            assert_eq!(from_paths.code(), tonic::Code::InvalidArgument, "{bad:?}");
            assert_eq!(from_paths.code(), new.code(), "{bad:?}");
            assert_eq!(from_paths.message(), new.message(), "{bad:?}");
        }

        let status =
            SubscriptionMatcher::from_paths(Some(&path("/interfaces")), &[]).expect_err("no paths");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(
            status.message().contains("no paths"),
            "{}",
            status.message()
        );
    }

    /// `JSON` is served as `PROTO`; `PROTO` and `JSON_IETF` as themselves.
    #[test]
    fn served_encoding_maps_json_to_proto() {
        assert_eq!(
            served_encoding(Encoding::Json as i32).unwrap(),
            Encoding::Proto
        );
        assert_eq!(
            served_encoding(Encoding::Proto as i32).unwrap(),
            Encoding::Proto
        );
        assert_eq!(
            served_encoding(Encoding::JsonIetf as i32).unwrap(),
            Encoding::JsonIetf
        );
        assert_eq!(
            served_encoding(9).expect_err("unknown").code(),
            tonic::Code::InvalidArgument
        );
    }

    #[test]
    fn bad_lists_are_invalid_arguments() {
        let empty = list(&[]);
        let status = SubscriptionMatcher::new(&empty).expect_err("no subscriptions");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);

        let mut unnamed = list(&["/interfaces"]);
        unnamed.subscription[0]
            .path
            .as_mut()
            .unwrap()
            .elem
            .push(proto::PathElem::default());
        let status = SubscriptionMatcher::new(&unnamed).expect_err("empty element name");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }
}
