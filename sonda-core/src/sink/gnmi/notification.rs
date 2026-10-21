//! Locating the value in one encoded `gnmi.Notification`, and re-encoding it
//! as JSON_IETF.

use std::io::{self, Write};
use std::ops::Range;

use bytes::Bytes;
use prost::encoding::{
    decode_key, decode_varint, encode_key, encode_varint, encoded_len_varint, key_len, skip_field,
    DecodeContext, WireType,
};

/// `Notification.update`.
const UPDATE: u32 = 4;

/// `Update.val`.
const VAL: u32 = 3;

/// `TypedValue.string_val`.
const STRING_VAL: u32 = 1;

/// `TypedValue.int_val`.
const INT_VAL: u32 = 2;

/// `TypedValue.uint_val`.
const UINT_VAL: u32 = 3;

/// `TypedValue.bool_val`.
const BOOL_VAL: u32 = 4;

/// `TypedValue.json_ietf_val`.
const JSON_IETF_VAL: u32 = 11;

/// `TypedValue.double_val`.
const DOUBLE_VAL: u32 = 14;

/// Room for the JSON text of any integer or double: the shortest decimal form
/// of an `f64`, written without an exponent, is under 400 characters.
const NUMBER_TEXT_CAPACITY: usize = 400;

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
            STRING_VAL => Self::String,
            INT_VAL => Self::Int,
            UINT_VAL => Self::Uint,
            BOOL_VAL => Self::Bool,
            JSON_IETF_VAL => Self::JsonIetf,
            DOUBLE_VAL => Self::Double,
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
    let (_, (_, val)) = value_field(encoded)?;
    let mut value = encoded.get(val.clone())?;
    let (field, wire_type) = decode_key(&mut value).ok()?;
    skip_field(wire_type, field, &mut value, DecodeContext::default()).ok()?;
    Some((val, ValueKind::from_field(field)))
}

/// The value of `encoded`'s first update, re-encoded as a `TypedValue`
/// holding only `json_ietf_val`: the value's RFC 7951 JSON text
/// ([`json_text`]). `encoded` is one encoded `gnmi.Notification`.
///
/// `None` when `encoded` is not a notification whose first update has a
/// value, or the value is of a kind with no JSON text here. Allocates once.
pub(crate) fn json_ietf_value(encoded: &[u8]) -> Option<Bytes> {
    let (val, _) = value_span(encoded)?;
    let mut number = [0u8; NUMBER_TEXT_CAPACITY];
    let text = json_text(encoded.get(val)?, &mut number)?;
    let len = delimited_len(JSON_IETF_VAL, text.len());
    let mut buf = Vec::with_capacity(len);
    write_json_ietf_val(&text, &mut buf)?;
    (buf.len() == len).then(|| Bytes::from(buf))
}

/// `encoded`, one encoded `gnmi.Notification`, with the contents of its
/// first update's `val` replaced by `value`, one encoded `TypedValue`. The
/// timestamp, the prefix, the update's path and every other field of the
/// notification and of the update are copied as they are, in place; the
/// update's length is recomputed. With [`json_ietf_value`] as `value`, this
/// is the notification in JSON_IETF.
///
/// `None` when `encoded` is not a notification whose first update has a
/// value. Allocates once.
pub(crate) fn with_value(encoded: &[u8], value: &[u8]) -> Option<Bytes> {
    let ((update_key, update), (val_key, val)) = value_field(encoded)?;
    let update_len =
        (val_key - update.start) + delimited_len(VAL, value.len()) + (update.end - val.end);
    let total = update_key + delimited_len(UPDATE, update_len) + (encoded.len() - update.end);
    let mut buf = Vec::with_capacity(total);
    buf.extend_from_slice(&encoded[..update_key]);
    encode_key(UPDATE, WireType::LengthDelimited, &mut buf);
    encode_varint(update_len as u64, &mut buf);
    buf.extend_from_slice(&encoded[update.start..val_key]);
    encode_key(VAL, WireType::LengthDelimited, &mut buf);
    encode_varint(value.len() as u64, &mut buf);
    buf.extend_from_slice(value);
    // The rest of the update, then the rest of the notification.
    buf.extend_from_slice(&encoded[val.end..]);
    (buf.len() == total).then(|| Bytes::from(buf))
}

/// The RFC 7951 JSON text of one value, ready to write.
enum JsonText<'a> {
    /// Text already formed: a number, a quoted integer, `true` or `false`, or
    /// a `json_ietf_val` carried as it is.
    Formed(&'a [u8]),
    /// A string, escaped as it is written.
    String(&'a str),
}

impl JsonText<'_> {
    /// The length of the text as written.
    fn len(&self) -> usize {
        match self {
            Self::Formed(text) => text.len(),
            Self::String(s) => {
                let mut counted = Counted(0);
                // Counting cannot fail: `Counted` accepts every write.
                let _ = serde_json::to_writer(&mut counted, s);
                counted.0
            }
        }
    }

    /// Append the text to `buf`. `None` if the string cannot be written.
    fn write(&self, buf: &mut Vec<u8>) -> Option<()> {
        match self {
            Self::Formed(text) => {
                buf.extend_from_slice(text);
                Some(())
            }
            Self::String(s) => serde_json::to_writer(buf, s).ok(),
        }
    }
}

/// A writer that keeps only the number of bytes written to it.
struct Counted(usize);

impl Write for Counted {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The RFC 7951 JSON text of `value`, one encoded `TypedValue`, by its first
/// field:
///
/// - `uint_val` and `int_val`: the decimal integer as a JSON string,
///   `"18446744073709551615"`; RFC 7951 carries 64-bit integers as strings.
/// - `double_val`: a JSON number in its shortest decimal form, without an
///   exponent; NaN and the infinities, which JSON numbers cannot hold, as the
///   JSON strings `"NaN"`, `"Infinity"` and `"-Infinity"`.
/// - `bool_val`: `true` or `false`.
/// - `string_val`: a JSON string, escaped as RFC 8259 requires.
/// - `json_ietf_val`: its text, as it is.
///
/// `None` for any other field, or a field that does not decode. Numbers are
/// formed in `number`.
fn json_text<'a>(
    mut value: &'a [u8],
    number: &'a mut [u8; NUMBER_TEXT_CAPACITY],
) -> Option<JsonText<'a>> {
    let (field, wire_type) = decode_key(&mut value).ok()?;
    let unused = {
        let mut cursor = &mut number[..];
        let formed = match (field, wire_type) {
            (STRING_VAL, WireType::LengthDelimited) => {
                return std::str::from_utf8(delimited_contents(&mut value)?)
                    .ok()
                    .map(JsonText::String);
            }
            (JSON_IETF_VAL, WireType::LengthDelimited) => {
                return delimited_contents(&mut value).map(JsonText::Formed);
            }
            // `int_val` is an `int64`: its varint is the two's complement.
            (INT_VAL, WireType::Varint) => {
                write!(cursor, "\"{}\"", decode_varint(&mut value).ok()? as i64)
            }
            (UINT_VAL, WireType::Varint) => {
                write!(cursor, "\"{}\"", decode_varint(&mut value).ok()?)
            }
            (BOOL_VAL, WireType::Varint) => {
                let text: &[u8] = if decode_varint(&mut value).ok()? != 0 {
                    b"true"
                } else {
                    b"false"
                };
                cursor.write_all(text)
            }
            (DOUBLE_VAL, WireType::SixtyFourBit) => {
                let double = f64::from_le_bytes(value.get(..8)?.try_into().ok()?);
                if double.is_finite() {
                    write!(cursor, "{double}")
                } else if double.is_nan() {
                    cursor.write_all(b"\"NaN\"")
                } else if double > 0.0 {
                    cursor.write_all(b"\"Infinity\"")
                } else {
                    cursor.write_all(b"\"-Infinity\"")
                }
            }
            _ => return None,
        };
        formed.ok()?;
        cursor.len()
    };
    let number: &'a [u8; NUMBER_TEXT_CAPACITY] = number;
    Some(JsonText::Formed(&number[..NUMBER_TEXT_CAPACITY - unused]))
}

/// The contents of the length-delimited field whose key `buf` has just been
/// read past, advancing `buf` past it. `None` when its length runs past
/// `buf`.
fn delimited_contents<'a>(buf: &mut &'a [u8]) -> Option<&'a [u8]> {
    let len = usize::try_from(decode_varint(buf).ok()?).ok()?;
    let contents = buf.get(..len)?;
    *buf = &buf[len..];
    Some(contents)
}

/// Append `TypedValue { json_ietf_val: text }`'s body to `buf`.
fn write_json_ietf_val(text: &JsonText<'_>, buf: &mut Vec<u8>) -> Option<()> {
    encode_key(JSON_IETF_VAL, WireType::LengthDelimited, buf);
    encode_varint(text.len() as u64, buf);
    text.write(buf)
}

/// Where a field's key starts, and the range of its contents.
type Field = (usize, Range<usize>);

/// The first update of `encoded`, one encoded `gnmi.Notification`, and its
/// first `val`. `None` when `encoded` is not a notification with such
/// fields.
fn value_field(encoded: &[u8]) -> Option<(Field, Field)> {
    let update = delimited(encoded, 0..encoded.len(), UPDATE)?;
    let val = delimited(encoded, update.1.clone(), VAL)?;
    Some((update, val))
}

/// The length of a length-delimited field `tag` holding `len` bytes.
fn delimited_len(tag: u32, len: usize) -> usize {
    key_len(tag) + encoded_len_varint(len as u64) + len
}

/// Where the first length-delimited field `tag` of the message at `message`
/// in `buf` starts, and the range of its contents. `None` when the message
/// has no such field, or a field before it, or its length, runs past the
/// message.
fn delimited(buf: &[u8], message: Range<usize>, tag: u32) -> Option<Field> {
    let end = message.end;
    let mut rest = buf.get(message)?;
    while !rest.is_empty() {
        let key = end - rest.len();
        let (field, wire_type) = decode_key(&mut rest).ok()?;
        if field == tag && wire_type == WireType::LengthDelimited {
            let len = usize::try_from(decode_varint(&mut rest).ok()?).ok()?;
            let start = end - rest.len();
            let stop = start.checked_add(len).filter(|&stop| stop <= end)?;
            return Some((key, start..stop));
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

    /// The `json_ietf_val` text of a notification's first update.
    fn json_text_of(notification: &proto::Notification) -> String {
        match notification
            .update
            .first()
            .and_then(|u| u.val.clone())
            .and_then(|v| v.value)
        {
            Some(Value::JsonIetfVal(text)) => String::from_utf8(text).expect("UTF-8 text"),
            other => panic!("not a json_ietf_val: {other:?}"),
        }
    }

    /// The JSON_IETF form of `buf`, [`with_value`] of [`json_ietf_value`],
    /// checked to copy everything but the value; returns its
    /// `json_ietf_val` text.
    fn converted_text(buf: &[u8]) -> String {
        let original = proto::Notification::decode(buf).expect("decodes");
        assert_ne!(original.timestamp, 0, "the premise: a timestamp to copy");
        assert!(original.prefix.is_some(), "the premise: a prefix to copy");
        let value = json_ietf_value(buf).expect("a value");
        let json = with_value(buf, &value).expect("a notification with a value");
        let converted = proto::Notification::decode(json.as_ref()).expect("the conversion decodes");
        assert_eq!(converted.timestamp, original.timestamp);
        assert_eq!(converted.prefix, original.prefix);
        assert_eq!(converted.update.len(), 1);
        assert_eq!(converted.update[0].path, original.update[0].path);
        assert_eq!(converted.delete, original.delete);
        json_text_of(&converted)
    }

    #[rustfmt::skip]
    #[rstest]
    #[case::uint_largest_encodable(GnmiValueType::Uint, 18_446_744_073_709_549_568.0, Value::UintVal(18_446_744_073_709_549_568), r#""18446744073709549568""#)]
    #[case::uint_zero(GnmiValueType::Uint, 0.0, Value::UintVal(0), r#""0""#)]
    #[case::int_min(GnmiValueType::Int, i64::MIN as f64, Value::IntVal(i64::MIN), r#""-9223372036854775808""#)]
    #[case::int(GnmiValueType::Int, -3.0, Value::IntVal(-3), r#""-3""#)]
    #[case::double(GnmiValueType::Double, 1.5, Value::DoubleVal(1.5), "1.5")]
    #[case::double_whole(GnmiValueType::Double, 2.0, Value::DoubleVal(2.0), "2")]
    #[case::bool_true(GnmiValueType::Bool, 1.0, Value::BoolVal(true), "true")]
    #[case::bool_false(GnmiValueType::Bool, 0.0, Value::BoolVal(false), "false")]
    #[case::string(GnmiValueType::String, 2.5, Value::StringVal("2.5".to_string()), r#""2.5""#)]
    #[case::enum_name(
        GnmiValueType::Enum(BTreeMap::from([(1, "UP".to_string())])),
        1.0,
        Value::StringVal("UP".to_string()),
        r#""UP""#
    )]
    #[case::enum_name_escaped(
        GnmiValueType::Enum(BTreeMap::from([(1, "say \"hi\" \\ then\ttab".to_string())])),
        1.0,
        Value::StringVal("say \"hi\" \\ then\ttab".to_string()),
        r#""say \"hi\" \\ then\ttab""#
    )]
    fn json_ietf_value_renders_each_value_type_by_rfc_7951(
        #[case] value_type: GnmiValueType,
        #[case] value: f64,
        #[case] proto_value: Value,
        #[case] expected: &str,
    ) {
        let buf = encoded(value_type, value);
        let original = proto::Notification::decode(buf.as_slice()).expect("decodes");
        assert_eq!(
            original.update[0].val.clone().and_then(|v| v.value),
            Some(proto_value.clone()),
            "the premise: the value the encoder wrote"
        );
        let text = converted_text(&buf);
        assert_eq!(text, expected);
        // The text is JSON, and a JSON parser reads back the value.
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("JSON text");
        match proto_value {
            Value::StringVal(s) => assert_eq!(parsed, serde_json::Value::String(s)),
            Value::UintVal(v) => assert_eq!(parsed, serde_json::Value::String(v.to_string())),
            Value::IntVal(v) => assert_eq!(parsed, serde_json::Value::String(v.to_string())),
            Value::BoolVal(v) => assert_eq!(parsed, serde_json::Value::Bool(v)),
            Value::DoubleVal(v) => assert_eq!(parsed.as_f64(), Some(v)),
            Value::JsonIetfVal(_) => unreachable!("the encoder writes no json_ietf_val"),
        }
    }

    /// The whole `uint_val` range is a JSON string, `u64::MAX` included. The
    /// encoder cannot write `u64::MAX`, which no `f64` holds, so the value is
    /// put into a notification the encoder wrote.
    #[test]
    fn uint_max_is_a_json_string() {
        let max = proto::TypedValue {
            value: Some(Value::UintVal(u64::MAX)),
        };
        let buf = with_value(&encoded(GnmiValueType::Uint, 7.0), &max.encode_to_vec())
            .expect("a notification with a value");
        assert_eq!(converted_text(&buf), r#""18446744073709551615""#);
    }

    /// A double is written in its shortest decimal form, which parses back
    /// to the same double, at both ends of the range.
    #[rstest]
    #[case::largest(f64::MAX)]
    #[case::smallest_subnormal(5e-324)]
    #[case::smallest_normal(f64::MIN_POSITIVE)]
    #[case::negative_zero(-0.0)]
    #[case::tenth(0.1)]
    #[case::beyond_exponent_notation(1e21)]
    fn a_double_is_its_shortest_decimal_form(#[case] value: f64) {
        let text = converted_text(&encoded(GnmiValueType::Double, value));
        assert!(
            !text.contains(['e', 'E']),
            "written without an exponent: {text}"
        );
        let parsed: f64 = serde_json::from_str(&text).expect("a JSON number");
        assert_eq!(parsed, value);
        assert_eq!(text, format!("{value}"));
    }

    /// NaN and the infinities, which no JSON number holds, are JSON strings.
    #[rstest]
    #[case::nan(f64::NAN, r#""NaN""#)]
    #[case::infinity(f64::INFINITY, r#""Infinity""#)]
    #[case::negative_infinity(f64::NEG_INFINITY, r#""-Infinity""#)]
    fn a_double_with_no_json_number_is_a_string(#[case] value: f64, #[case] expected: &str) {
        let buf = encoded(GnmiValueType::Double, value);
        assert!(
            matches!(
                proto::Notification::decode(buf.as_slice()).expect("decodes").update[0]
                    .val
                    .clone()
                    .and_then(|v| v.value),
                Some(Value::DoubleVal(v)) if v.to_bits() == value.to_bits()
            ),
            "the premise: the encoder writes the double as it is"
        );
        assert_eq!(converted_text(&buf), expected);
    }

    /// `with_value` replaces the update's value and copies every other field
    /// of the notification and the update, before it and after it.
    #[test]
    fn with_value_copies_every_other_field() {
        let path = proto::Path {
            elem: vec![proto::PathElem {
                name: "up".to_string(),
                key: BTreeMap::new(),
            }],
            ..Default::default()
        };
        let original = proto::Notification {
            timestamp: 9,
            prefix: Some(proto::Path {
                origin: "openconfig".to_string(),
                target: "rtr-1".to_string(),
                ..Default::default()
            }),
            update: vec![proto::Update {
                path: Some(path.clone()),
                val: Some(proto::TypedValue {
                    value: Some(Value::UintVal(1)),
                }),
            }],
            delete: vec![path.clone()],
            atomic: true,
        };
        let replacement = proto::TypedValue {
            value: Some(Value::StringVal(
                "a longer value than one varint".to_string(),
            )),
        };
        let spliced = with_value(&original.encode_to_vec(), &replacement.encode_to_vec())
            .expect("a notification with a value");
        let decoded = proto::Notification::decode(spliced.as_ref()).expect("decodes");
        assert_eq!(
            decoded,
            proto::Notification {
                update: vec![proto::Update {
                    path: Some(path),
                    val: Some(replacement),
                }],
                ..original
            }
        );
    }

    /// A buffer that is not a notification with a value, and a value of a
    /// kind with no JSON text, have no JSON_IETF form.
    #[test]
    fn bytes_without_a_json_value_have_no_json_ietf_form() {
        let buf = encoded(GnmiValueType::Uint, 7.0);
        for bytes in [&b"bytes"[..], &[], &buf[..buf.len() - 1]] {
            assert_eq!(json_ietf_value(bytes), None);
            assert_eq!(with_value(bytes, b""), None);
        }
        // `bytes_val` (field 5) holding "x".
        let bytes_val = with_value(&buf, &[(5 << 3) | 2, 1, b'x']).expect("spliced");
        assert!(
            value_span(&bytes_val).is_some_and(|(_, kind)| kind == ValueKind::Other(5)),
            "the premise: a notification whose value is bytes_val"
        );
        assert_eq!(json_ietf_value(&bytes_val), None);
    }
}
