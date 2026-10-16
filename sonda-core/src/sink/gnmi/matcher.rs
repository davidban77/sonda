//! Which series a gNMI `SubscriptionList` selects.

use tonic::Status;

use crate::encoder::gnmi::proto::{self, subscription_list, Encoding, SubscriptionMode};

/// The subscribed paths of one `SubscriptionList`, each joined to the list's
/// prefix.
///
/// A subscription path is the prefix's elements followed by the
/// subscription's own, with the subscription's origin (the prefix's when the
/// subscription has none) and the prefix's target.
#[derive(Debug, Clone)]
pub struct SubscriptionMatcher {
    paths: Vec<proto::Path>,
}

impl SubscriptionMatcher {
    /// Join each subscription in `list` to the list's prefix.
    ///
    /// # Errors
    ///
    /// - `INVALID_ARGUMENT` when the list has no subscriptions, a path has
    ///   an element with an empty name, or the list's encoding or mode, or in
    ///   a `STREAM` list a subscription's mode, is a number gNMI does not
    ///   define. A `ONCE` list's subscription modes are not read.
    /// - `UNIMPLEMENTED`, naming what was asked for, for a `POLL` list, an
    ///   `ON_CHANGE` subscription in a `STREAM` list, an encoding other than
    ///   `PROTO`, or a path element named `*` or `...`.
    pub fn new(list: &proto::SubscriptionList) -> Result<Self, Status> {
        let encoding = Encoding::try_from(list.encoding).map_err(|_| {
            Status::invalid_argument(format!("unknown gnmi encoding {}", list.encoding))
        })?;
        if encoding != Encoding::Proto {
            return Err(Status::unimplemented(format!(
                "encoding {} is not supported; this target encodes PROTO only",
                encoding_name(encoding)
            )));
        }
        let mode = subscription_list::Mode::try_from(list.mode).map_err(|_| {
            Status::invalid_argument(format!("unknown subscription list mode {}", list.mode))
        })?;
        if mode == subscription_list::Mode::Poll {
            return Err(Status::unimplemented(
                "subscription list mode POLL is not supported; use STREAM or ONCE",
            ));
        }
        if list.subscription.is_empty() {
            return Err(Status::invalid_argument(
                "the subscription list has no subscriptions",
            ));
        }
        let prefix = list.prefix.clone().unwrap_or_default();
        let mut paths = Vec::with_capacity(list.subscription.len());
        for subscription in &list.subscription {
            if mode == subscription_list::Mode::Stream {
                let sub_mode = SubscriptionMode::try_from(subscription.mode).map_err(|_| {
                    Status::invalid_argument(format!(
                        "unknown subscription mode {}",
                        subscription.mode
                    ))
                })?;
                if sub_mode == SubscriptionMode::OnChange {
                    return Err(Status::unimplemented(
                        "subscription mode ON_CHANGE is not supported; use SAMPLE or \
                         TARGET_DEFINED",
                    ));
                }
            }
            let own = subscription.path.clone().unwrap_or_default();
            let mut elem = prefix.elem.clone();
            elem.extend(own.elem);
            for element in &elem {
                match element.name.as_str() {
                    "" => {
                        return Err(Status::invalid_argument(
                            "a subscription path has an element with an empty name",
                        ))
                    }
                    "*" | "..." => {
                        return Err(Status::unimplemented(format!(
                            "path element wildcard {:?} is not supported; use a key \
                             wildcard such as [name=*]",
                            element.name
                        )))
                    }
                    _ => {}
                }
            }
            let origin = if own.origin.is_empty() {
                prefix.origin.clone()
            } else {
                own.origin
            };
            paths.push(proto::Path {
                origin,
                elem,
                target: prefix.target.clone(),
            });
        }
        Ok(Self { paths })
    }

    /// True if `series_path` is under any subscription path. Prefix match on elements;
    /// a subscription key of "*" or an absent key matches any value.
    ///
    /// An empty subscription origin or target matches any; otherwise it
    /// must equal the series'.
    pub fn matches(&self, series_path: &proto::Path) -> bool {
        self.paths.iter().any(|path| is_under(series_path, path))
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

/// Whether `series` is `subscribed` or below it.
fn is_under(series: &proto::Path, subscribed: &proto::Path) -> bool {
    (subscribed.origin.is_empty() || subscribed.origin == series.origin)
        && (subscribed.target.is_empty() || subscribed.target == series.target)
        && subscribed.elem.len() <= series.elem.len()
        && subscribed
            .elem
            .iter()
            .zip(&series.elem)
            .all(|(wanted, elem)| {
                wanted.name == elem.name
                    && wanted
                        .key
                        .iter()
                        .all(|(k, v)| v == "*" || elem.key.get(k) == Some(v))
            })
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
    #[case::poll_list(subscription_list::Mode::Poll as i32, SubscriptionMode::Sample as i32, Encoding::Proto as i32, tonic::Code::Unimplemented, "POLL")]
    #[case::on_change(subscription_list::Mode::Stream as i32, SubscriptionMode::OnChange as i32, Encoding::Proto as i32, tonic::Code::Unimplemented, "ON_CHANGE")]
    #[case::json_encoding(subscription_list::Mode::Stream as i32, SubscriptionMode::Sample as i32, Encoding::Json as i32, tonic::Code::Unimplemented, "JSON")]
    #[case::json_ietf_encoding(subscription_list::Mode::Stream as i32, SubscriptionMode::Sample as i32, Encoding::JsonIetf as i32, tonic::Code::Unimplemented, "JSON_IETF")]
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

    /// The modes this target serves are accepted, and in a ONCE list the
    /// per-subscription mode does not apply.
    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::stream_sample(subscription_list::Mode::Stream, SubscriptionMode::Sample)]
    #[case::stream_target_defined(subscription_list::Mode::Stream, SubscriptionMode::TargetDefined)]
    #[case::once(subscription_list::Mode::Once, SubscriptionMode::OnChange)]
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
    #[case::element_wildcard("/interfaces/*/state", tonic::Code::Unimplemented)]
    #[case::multi_level_wildcard("/interfaces/.../state", tonic::Code::Unimplemented)]
    fn element_wildcards_are_refused(#[case] p: &str, #[case] code: tonic::Code) {
        let status = SubscriptionMatcher::new(&list(&[p])).expect_err("refused");
        assert_eq!(status.code(), code, "{}", status.message());
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
