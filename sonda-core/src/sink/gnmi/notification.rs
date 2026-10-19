//! Locating the value in one encoded `gnmi.Notification`.

use std::ops::Range;

use prost::encoding::{decode_key, decode_varint, skip_field, DecodeContext, WireType};

/// `Notification.update`.
const UPDATE: u32 = 4;

/// `Update.val`.
const VAL: u32 = 3;

/// The `TypedValue` variant a series' value carries, named by the field
/// number of the value's oneof member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ValueKind {
    /// `string_val` (field 1): the gnmi encoder's `string` and `enum` values.
    String,
    /// `int_val` (field 2).
    Int,
    /// `uint_val` (field 3).
    Uint,
    /// `bool_val` (field 4).
    Bool,
    /// `double_val` (field 14).
    Double,
    /// `json_ietf_val` (field 11).
    JsonIetf,
    /// Any other field number.
    Other(u32),
}

impl ValueKind {
    /// The kind named by `TypedValue` field number `field`.
    fn from_field(field: u32) -> Self {
        match field {
            1 => Self::String,
            2 => Self::Int,
            3 => Self::Uint,
            4 => Self::Bool,
            11 => Self::JsonIetf,
            14 => Self::Double,
            other => Self::Other(other),
        }
    }
}

/// The byte range of the first update's `val` in `encoded`, one encoded
/// `gnmi.Notification`, and the kind of its first field. The range holds the
/// encoded `TypedValue` without its key and length.
///
/// Reads the keys and lengths before the value and skips every other field
/// undecoded; the update's path is skipped, not decoded. Allocates nothing.
///
/// `None` when `encoded` is not a notification whose first update has a
/// `val` holding a well-formed first field.
pub(crate) fn value_span(encoded: &[u8]) -> Option<(Range<usize>, ValueKind)> {
    let update = delimited(encoded, 0..encoded.len(), UPDATE)?;
    let val = delimited(encoded, update, VAL)?;
    let mut value = encoded.get(val.clone())?;
    let (field, wire_type) = decode_key(&mut value).ok()?;
    skip_field(wire_type, field, &mut value, DecodeContext::default()).ok()?;
    Some((val, ValueKind::from_field(field)))
}

/// The range in `buf` of the contents of the first length-delimited field
/// `tag` of the message at `message`. `None` when the message has no such
/// field, or a field before it, or its length, runs past the message.
fn delimited(buf: &[u8], message: Range<usize>, tag: u32) -> Option<Range<usize>> {
    let end = message.end;
    let mut rest = buf.get(message)?;
    while !rest.is_empty() {
        let (field, wire_type) = decode_key(&mut rest).ok()?;
        if field == tag && wire_type == WireType::LengthDelimited {
            let len = usize::try_from(decode_varint(&mut rest).ok()?).ok()?;
            let start = end - rest.len();
            let stop = start.checked_add(len).filter(|&stop| stop <= end)?;
            return Some(start..stop);
        }
        skip_field(wire_type, field, &mut rest, DecodeContext::default()).ok()?;
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use prost::Message;
    use rstest::rstest;

    use super::*;
    use crate::encoder::gnmi::proto::{self, typed_value::Value};
    use crate::encoder::gnmi::{GnmiEncoder, GnmiEncoderConfig, GnmiValueType};
    use crate::encoder::Encoder;
    use crate::model::metric::{Labels, MetricEvent};
    use crate::sink::gnmi::target::PathKey;

    /// `in_octets{ifName=Gi0/0/0, device=rtr-1}` at `value`, encoded with
    /// `in_octets` typed as `value_type`.
    fn encoded(value_type: GnmiValueType, value: f64) -> Vec<u8> {
        let encoder = GnmiEncoder::new(&GnmiEncoderConfig {
            path: Some("/interfaces/interface[name={ifName}]/state/counters/{name}".to_string()),
            values: HashMap::from([("in_octets".to_string(), value_type)]),
            ..GnmiEncoderConfig::default()
        })
        .expect("valid encoder");
        let event = MetricEvent::new(
            "in_octets".to_string(),
            value,
            Labels::from_pairs(&[("device", "rtr-1"), ("ifName", "Gi0/0/0")]).expect("labels"),
        )
        .expect("valid event");
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encodes");
        buf
    }

    #[rustfmt::skip]
    #[rstest]
    #[case::uint(GnmiValueType::Uint, 7.0, Value::UintVal(7), ValueKind::Uint)]
    #[case::uint_zero(GnmiValueType::Uint, 0.0, Value::UintVal(0), ValueKind::Uint)]
    #[case::int(GnmiValueType::Int, -3.0, Value::IntVal(-3), ValueKind::Int)]
    #[case::double(GnmiValueType::Double, 1.5, Value::DoubleVal(1.5), ValueKind::Double)]
    #[case::bool_false(GnmiValueType::Bool, 0.0, Value::BoolVal(false), ValueKind::Bool)]
    #[case::string(GnmiValueType::String, 2.5, Value::StringVal("2.5".to_string()), ValueKind::String)]
    #[case::enumeration(
        GnmiValueType::Enum(BTreeMap::from([(1, "UP".to_string()), (2, "DOWN".to_string())])),
        2.0,
        Value::StringVal("DOWN".to_string()),
        ValueKind::String
    )]
    fn value_span_locates_the_encoders_value(
        #[case] value_type: GnmiValueType,
        #[case] value: f64,
        #[case] expected: Value,
        #[case] kind: ValueKind,
    ) {
        let buf = encoded(value_type, value);
        let (range, found) = value_span(&buf).expect("a located value");
        assert_eq!(found, kind);
        let located = proto::TypedValue::decode(&buf[range]).expect("a typed value");
        assert_eq!(located.value, Some(expected));
        assert_eq!(
            PathKey::decode(&buf).expect("a path").path().to_string(),
            "openconfig:/interfaces/interface[name=Gi0/0/0]/state/counters/in-octets",
            "the same bytes carry the path PathKey::decode reads"
        );
    }

    #[test]
    fn value_span_is_the_value_and_nothing_else() {
        let buf = encoded(GnmiValueType::Uint, 7.0);
        let (range, _) = value_span(&buf).expect("a located value");
        let notification = proto::Notification::decode(buf.as_slice()).expect("decodes");
        let val = notification.update[0].val.clone().expect("a value");
        assert_eq!(&buf[range], val.encode_to_vec().as_slice());
    }

    #[test]
    fn bytes_that_are_not_a_notification_with_a_value_are_none() {
        let buf = encoded(GnmiValueType::String, 2.5);
        assert_eq!(value_span(b"bytes"), None);
        assert_eq!(value_span(&[]), None);
        assert_eq!(value_span(&buf[..buf.len() - 1]), None, "a truncated value");
        let no_update = proto::Notification {
            timestamp: 1,
            ..Default::default()
        };
        assert_eq!(value_span(&no_update.encode_to_vec()), None);
        let no_value = proto::Notification {
            update: vec![proto::Update {
                path: Some(proto::Path::default()),
                val: None,
            }],
            ..Default::default()
        };
        assert_eq!(value_span(&no_value.encode_to_vec()), None);
    }
}
