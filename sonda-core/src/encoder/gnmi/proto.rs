//! Hand-written prost types for the subset of `gnmi.proto` that Sonda uses.
//!
//! Field numbers are copied from `openconfig/gnmi` `proto/gnmi/gnmi.proto`
//! (package `gnmi`). Fields not listed here are absent from the structs and
//! are skipped on decode.

use std::collections::BTreeMap;
use std::fmt;

/// A data-tree path.
///
/// Corresponds to `gnmi.Path`. The deprecated `element` field (1) is omitted.
#[derive(Clone, PartialEq, Eq, Hash, prost::Message)]
pub struct Path {
    /// Label disambiguating the schema the path belongs to.
    #[prost(string, tag = "2")]
    pub origin: String,
    /// Elements of the path, root first.
    #[prost(message, repeated, tag = "3")]
    pub elem: Vec<PathElem>,
    /// Name of the target the path refers to.
    #[prost(string, tag = "4")]
    pub target: String,
}

/// One element of a [`Path`] with its list keys.
///
/// Corresponds to `gnmi.PathElem`. Keys are held in a `BTreeMap` so the
/// encoded bytes are deterministic.
#[derive(Clone, PartialEq, Eq, Hash, prost::Message)]
pub struct PathElem {
    /// Element name.
    #[prost(string, tag = "1")]
    pub name: String,
    /// Key name to key value.
    #[prost(btree_map = "string, string", tag = "2")]
    pub key: BTreeMap<String, String>,
}

/// An explicitly typed value.
///
/// Corresponds to `gnmi.TypedValue`, restricted to the scalar variants Sonda
/// emits plus `json_ietf_val`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct TypedValue {
    /// The value payload.
    #[prost(oneof = "typed_value::Value", tags = "1, 2, 3, 4, 11, 14")]
    pub value: Option<typed_value::Value>,
}

/// Oneof variants for [`TypedValue`].
pub mod typed_value {
    /// The `value` oneof of `gnmi.TypedValue`.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Value {
        /// `string_val = 1`.
        #[prost(string, tag = "1")]
        StringVal(String),
        /// `int_val = 2`.
        #[prost(int64, tag = "2")]
        IntVal(i64),
        /// `uint_val = 3`.
        #[prost(uint64, tag = "3")]
        UintVal(u64),
        /// `bool_val = 4`.
        #[prost(bool, tag = "4")]
        BoolVal(bool),
        /// `json_ietf_val = 11`, RFC 7951 JSON text.
        #[prost(bytes = "vec", tag = "11")]
        JsonIetfVal(Vec<u8>),
        /// `double_val = 14`.
        #[prost(double, tag = "14")]
        DoubleVal(f64),
    }
}

/// A path and its new value.
///
/// Corresponds to `gnmi.Update`. The deprecated `value` (2) and `duplicates`
/// (4) fields are omitted.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Update {
    /// The path the value belongs to, relative to the notification prefix.
    #[prost(message, optional, tag = "1")]
    pub path: Option<Path>,
    /// The typed value.
    #[prost(message, optional, tag = "3")]
    pub val: Option<TypedValue>,
}

/// A timestamped set of updates and deletes.
///
/// Corresponds to `gnmi.Notification`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Notification {
    /// Nanoseconds since the Unix epoch.
    #[prost(int64, tag = "1")]
    pub timestamp: i64,
    /// Prefix applied to every path in `update` and `delete`.
    #[prost(message, optional, tag = "2")]
    pub prefix: Option<Path>,
    /// Values that changed.
    #[prost(message, repeated, tag = "4")]
    pub update: Vec<Update>,
    /// Paths that were deleted.
    #[prost(message, repeated, tag = "5")]
    pub delete: Vec<Path>,
    /// Whether the updates must be applied as one unit.
    #[prost(bool, tag = "6")]
    pub atomic: bool,
}

/// A client message on the `Subscribe` stream.
///
/// Corresponds to `gnmi.SubscribeRequest`. `extension` (5) is omitted.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SubscribeRequest {
    /// The request payload.
    #[prost(oneof = "subscribe_request::Request", tags = "1, 3")]
    pub request: Option<subscribe_request::Request>,
}

/// Oneof variants for [`SubscribeRequest`].
pub mod subscribe_request {
    /// The `request` oneof of `gnmi.SubscribeRequest`.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Request {
        /// `subscribe = 1`.
        #[prost(message, tag = "1")]
        Subscribe(super::SubscriptionList),
        /// `poll = 3`.
        #[prost(message, tag = "3")]
        Poll(super::Poll),
    }
}

/// Trigger for a polled update.
///
/// Corresponds to `gnmi.Poll`, which has no fields.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Poll {}

/// A server message on the `Subscribe` stream.
///
/// Corresponds to `gnmi.SubscribeResponse`. The deprecated `error` (4) and
/// `extension` (5) fields are omitted.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SubscribeResponse {
    /// The response payload.
    #[prost(oneof = "subscribe_response::Response", tags = "1, 3")]
    pub response: Option<subscribe_response::Response>,
}

/// Oneof variants for [`SubscribeResponse`].
pub mod subscribe_response {
    /// The `response` oneof of `gnmi.SubscribeResponse`.
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Response {
        /// `update = 1`.
        #[prost(message, tag = "1")]
        Update(super::Notification),
        /// `sync_response = 3`.
        #[prost(bool, tag = "3")]
        SyncResponse(bool),
    }
}

/// The set of subscriptions a client requests.
///
/// Corresponds to `gnmi.SubscriptionList`. `qos` (4), `allow_aggregation`
/// (6) and `use_models` (7) are omitted.
#[derive(Clone, PartialEq, prost::Message)]
pub struct SubscriptionList {
    /// Prefix applied to every subscription path.
    #[prost(message, optional, tag = "1")]
    pub prefix: Option<Path>,
    /// The subscriptions.
    #[prost(message, repeated, tag = "2")]
    pub subscription: Vec<Subscription>,
    /// Stream, once or poll. See [`subscription_list::Mode`].
    #[prost(enumeration = "subscription_list::Mode", tag = "5")]
    pub mode: i32,
    /// Requested encoding. See [`Encoding`].
    #[prost(enumeration = "Encoding", tag = "8")]
    pub encoding: i32,
    /// Skip the initial sync when true.
    #[prost(bool, tag = "9")]
    pub updates_only: bool,
}

/// Nested types of [`SubscriptionList`].
pub mod subscription_list {
    /// `gnmi.SubscriptionList.Mode`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
    #[repr(i32)]
    pub enum Mode {
        /// `STREAM = 0`.
        Stream = 0,
        /// `ONCE = 1`.
        Once = 1,
        /// `POLL = 2`.
        Poll = 2,
    }
}

/// One subscribed path and how to sample it.
///
/// Corresponds to `gnmi.Subscription`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Subscription {
    /// The subscribed path, relative to the list prefix.
    #[prost(message, optional, tag = "1")]
    pub path: Option<Path>,
    /// See [`SubscriptionMode`].
    #[prost(enumeration = "SubscriptionMode", tag = "2")]
    pub mode: i32,
    /// Nanoseconds between samples in `SAMPLE` mode.
    #[prost(uint64, tag = "3")]
    pub sample_interval: u64,
    /// Skip unchanged values in `SAMPLE` mode.
    #[prost(bool, tag = "4")]
    pub suppress_redundant: bool,
    /// Nanoseconds between forced re-sends.
    #[prost(uint64, tag = "5")]
    pub heartbeat_interval: u64,
}

/// `gnmi.CapabilityRequest`. `extension` (1) is omitted.
#[derive(Clone, PartialEq, prost::Message)]
pub struct CapabilityRequest {}

/// `gnmi.CapabilityResponse`. `extension` (4) is omitted.
#[derive(Clone, PartialEq, prost::Message)]
pub struct CapabilityResponse {
    /// Schema models the target supports.
    #[prost(message, repeated, tag = "1")]
    pub supported_models: Vec<ModelData>,
    /// Encodings the target supports. See [`Encoding`].
    #[prost(enumeration = "Encoding", repeated, tag = "2")]
    pub supported_encodings: Vec<i32>,
    /// The `gNMI_version` field.
    #[prost(string, tag = "3")]
    pub gnmi_version: String,
}

/// `gnmi.ModelData`.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ModelData {
    /// Model name.
    #[prost(string, tag = "1")]
    pub name: String,
    /// Publishing organization.
    #[prost(string, tag = "2")]
    pub organization: String,
    /// Semantic version.
    #[prost(string, tag = "3")]
    pub version: String,
}

/// `gnmi.Encoding`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum Encoding {
    /// `JSON = 0`.
    Json = 0,
    /// `BYTES = 1`.
    Bytes = 1,
    /// `PROTO = 2`.
    Proto = 2,
    /// `ASCII = 3`.
    Ascii = 3,
    /// `JSON_IETF = 4`.
    JsonIetf = 4,
}

/// `gnmi.SubscriptionMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum SubscriptionMode {
    /// `TARGET_DEFINED = 0`.
    TargetDefined = 0,
    /// `ON_CHANGE = 1`.
    OnChange = 1,
    /// `SAMPLE = 2`.
    Sample = 2,
}

/// Renders the path in gNMI string form: `origin:/a/b[k=v]/c`.
///
/// The `origin:` prefix appears only when `origin` is non-empty, keys appear
/// in sorted order, and `]` and `\` inside key values are backslash-escaped.
/// An empty element list renders as `/`. `target` is not rendered.
/// [`super::path::parse_client_path`] parses this form back.
impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.origin.is_empty() {
            write!(f, "{}:", self.origin)?;
        }
        if self.elem.is_empty() {
            return f.write_str("/");
        }
        for elem in &self.elem {
            write!(f, "/{}", elem.name)?;
            for (k, v) in &elem.key {
                write!(f, "[{k}=")?;
                for c in v.chars() {
                    if c == ']' || c == '\\' {
                        f.write_str("\\")?;
                    }
                    write!(f, "{c}")?;
                }
                f.write_str("]")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    fn path_elem(name: &str, keys: &[(&str, &str)]) -> PathElem {
        PathElem {
            name: name.to_string(),
            key: keys
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    // Expected bytes are assembled by hand from gnmi.proto: a key byte is
    // (field_number << 3) | wire_type, with wire type 0 = varint,
    // 1 = 64-bit, 2 = length-delimited.

    #[test]
    fn typed_value_double_uses_field_14() {
        let tv = TypedValue {
            value: Some(typed_value::Value::DoubleVal(1.0)),
        };
        // (14 << 3) | 1 = 0x71, then 1.0f64 little-endian.
        assert_eq!(tv.encode_to_vec(), vec![0x71, 0, 0, 0, 0, 0, 0, 0xf0, 0x3f]);
    }

    #[test]
    fn typed_value_scalar_variants_use_their_field_numbers() {
        let cases: [(typed_value::Value, Vec<u8>); 5] = [
            (
                typed_value::Value::StringVal("a".into()),
                vec![0x0a, 1, b'a'],
            ),
            (typed_value::Value::IntVal(-1), {
                let mut v = vec![0x10];
                v.extend([0xff; 9]);
                v.push(0x01);
                v
            }),
            (typed_value::Value::UintVal(5), vec![0x18, 5]),
            (typed_value::Value::BoolVal(true), vec![0x20, 1]),
            (
                typed_value::Value::JsonIetfVal(b"1".to_vec()),
                vec![0x5a, 1, b'1'],
            ),
        ];
        for (value, expected) in cases {
            let tv = TypedValue {
                value: Some(value.clone()),
            };
            assert_eq!(tv.encode_to_vec(), expected, "{value:?}");
        }
    }

    #[test]
    fn notification_update_and_path_use_their_field_numbers() {
        let n = Notification {
            timestamp: 1,
            prefix: Some(Path {
                origin: "o".into(),
                elem: vec![],
                target: "t".into(),
            }),
            update: vec![Update {
                path: Some(Path {
                    origin: String::new(),
                    elem: vec![path_elem("e", &[("k", "v")])],
                    target: String::new(),
                }),
                val: Some(TypedValue {
                    value: Some(typed_value::Value::BoolVal(true)),
                }),
            }],
            delete: vec![],
            atomic: true,
        };
        #[rustfmt::skip]
        let expected = vec![
            0x08, 0x01,                                  // timestamp = 1
            0x12, 0x06,                                  // prefix (2), len 6
                0x12, 0x01, b'o',                        //   origin (2)
                0x22, 0x01, b't',                        //   target (4)
            0x22, 0x13,                                  // update (4), len 19
                0x0a, 0x0d,                              //   path (1), len 13
                    0x1a, 0x0b,                          //     elem (3), len 11
                        0x0a, 0x01, b'e',                //       name (1)
                        0x12, 0x06,                      //       key (2) map entry
                            0x0a, 0x01, b'k',            //         key
                            0x12, 0x01, b'v',            //         value
                0x1a, 0x02,                              //   val (3), len 2
                    0x20, 0x01,                          //     bool_val (4)
            0x30, 0x01,                                  // atomic (6)
        ];
        assert_eq!(n.encode_to_vec(), expected);
        assert_eq!(Notification::decode(expected.as_slice()).unwrap(), n);
    }

    #[test]
    fn notification_delete_uses_field_5() {
        let n = Notification {
            timestamp: 0,
            prefix: None,
            update: vec![],
            delete: vec![Path {
                origin: "o".into(),
                elem: vec![],
                target: String::new(),
            }],
            atomic: false,
        };
        assert_eq!(n.encode_to_vec(), vec![0x2a, 0x03, 0x12, 0x01, b'o']);
    }

    #[test]
    fn subscribe_request_and_list_use_their_field_numbers() {
        let req = SubscribeRequest {
            request: Some(subscribe_request::Request::Subscribe(SubscriptionList {
                prefix: Some(Path {
                    origin: String::new(),
                    elem: vec![],
                    target: "t".into(),
                }),
                subscription: vec![Subscription {
                    path: None,
                    mode: SubscriptionMode::Sample as i32,
                    sample_interval: 7,
                    suppress_redundant: true,
                    heartbeat_interval: 9,
                }],
                mode: subscription_list::Mode::Once as i32,
                encoding: Encoding::Proto as i32,
                updates_only: true,
            })),
        };
        #[rustfmt::skip]
        let expected = vec![
            0x0a, 0x15,                      // subscribe (1), len 21
                0x0a, 0x03,                  //   prefix (1)
                    0x22, 0x01, b't',        //     target (4)
                0x12, 0x08,                  //   subscription (2), len 8
                    0x10, 0x02,              //     mode (2) = SAMPLE
                    0x18, 0x07,              //     sample_interval (3)
                    0x20, 0x01,              //     suppress_redundant (4)
                    0x28, 0x09,              //     heartbeat_interval (5)
                0x28, 0x01,                  //   mode (5) = ONCE
                0x40, 0x02,                  //   encoding (8) = PROTO
                0x48, 0x01,                  //   updates_only (9)
        ];
        assert_eq!(req.encode_to_vec(), expected);

        let poll = SubscribeRequest {
            request: Some(subscribe_request::Request::Poll(Poll {})),
        };
        assert_eq!(poll.encode_to_vec(), vec![0x1a, 0x00]);
    }

    #[test]
    fn subscribe_response_uses_fields_1_and_3() {
        let sync = SubscribeResponse {
            response: Some(subscribe_response::Response::SyncResponse(true)),
        };
        assert_eq!(sync.encode_to_vec(), vec![0x18, 0x01]);

        let update = SubscribeResponse {
            response: Some(subscribe_response::Response::Update(Notification {
                timestamp: 2,
                prefix: None,
                update: vec![],
                delete: vec![],
                atomic: false,
            })),
        };
        assert_eq!(update.encode_to_vec(), vec![0x0a, 0x02, 0x08, 0x02]);
    }

    #[test]
    fn capability_response_uses_its_field_numbers() {
        let resp = CapabilityResponse {
            supported_models: vec![ModelData {
                name: "n".into(),
                organization: "o".into(),
                version: "v".into(),
            }],
            supported_encodings: vec![Encoding::Proto as i32, Encoding::JsonIetf as i32],
            gnmi_version: "0.10.0".into(),
        };
        #[rustfmt::skip]
        let expected = vec![
            0x0a, 0x09,                                      // supported_models (1)
                0x0a, 0x01, b'n', 0x12, 0x01, b'o', 0x1a, 0x01, b'v',
            0x12, 0x02, 0x02, 0x04,                          // supported_encodings (2), packed
            0x1a, 0x06, b'0', b'.', b'1', b'0', b'.', b'0',  // gNMI_version (3)
        ];
        assert_eq!(resp.encode_to_vec(), expected);
        assert!(CapabilityRequest {}.encode_to_vec().is_empty());
    }

    #[test]
    fn enum_values_match_gnmi_proto() {
        assert_eq!(Encoding::Json as i32, 0);
        assert_eq!(Encoding::Bytes as i32, 1);
        assert_eq!(Encoding::Proto as i32, 2);
        assert_eq!(Encoding::Ascii as i32, 3);
        assert_eq!(Encoding::JsonIetf as i32, 4);
        assert_eq!(SubscriptionMode::TargetDefined as i32, 0);
        assert_eq!(SubscriptionMode::OnChange as i32, 1);
        assert_eq!(SubscriptionMode::Sample as i32, 2);
        assert_eq!(subscription_list::Mode::Stream as i32, 0);
        assert_eq!(subscription_list::Mode::Once as i32, 1);
        assert_eq!(subscription_list::Mode::Poll as i32, 2);
    }

    #[test]
    fn path_display_renders_origin_keys_and_escapes() {
        let path = Path {
            origin: "openconfig".into(),
            elem: vec![
                path_elem("interfaces", &[]),
                path_elem("interface", &[("name", "Gi0/0/0"), ("b", "x]y\\z")]),
                path_elem("state", &[]),
            ],
            target: "ignored".into(),
        };
        assert_eq!(
            path.to_string(),
            r"openconfig:/interfaces/interface[b=x\]y\\z][name=Gi0/0/0]/state"
        );
        assert_eq!(Path::default().to_string(), "/");
    }
}
