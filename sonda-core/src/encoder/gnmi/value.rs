//! The gNMI value type: which `TypedValue` variant a metric's value is
//! encoded as, and its YAML shape.
//!
//! Compiled with or without the `gnmi` feature: a pack's `gnmi:` hints
//! carry it.

use std::collections::BTreeMap;

/// The `TypedValue` variant a metric's value is encoded as.
///
/// YAML forms: `uint`, `int`, `double`, `bool`, `string`, and
/// `{ enum: { 1: UP, 2: DOWN } }`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "config",
    serde(from = "GnmiValueTypeWire", into = "GnmiValueTypeWire")
)]
pub enum GnmiValueType {
    /// `uint_val`: the value rounded to the nearest integer (halves away
    /// from zero). A non-finite value, or one whose rounded value is outside
    /// the `u64` range (so any value at or below -0.5), rejects the event.
    Uint,
    /// `int_val`: the value rounded to the nearest integer (halves away from
    /// zero). A non-finite value, or one whose rounded value is outside the
    /// `i64` range, rejects the event.
    Int,
    /// `double_val` (field 14).
    Double,
    /// `bool_val`: `true` when the value rounded to the nearest integer is not
    /// 0, so float noise such as `1.2e-16` is `false`. A non-finite value
    /// rejects the event.
    Bool,
    /// `string_val`: the shortest decimal text that round-trips the `f64`.
    String,
    /// `string_val` looked up by the value rounded to the nearest integer, as
    /// for [`Int`](Self::Int), so float noise such as `0.9999999999999999`
    /// still selects code `1`. A value with no entry, or a non-finite value,
    /// rejects the event.
    Enum(BTreeMap<i64, String>),
}

/// YAML shape of [`GnmiValueType`]: a bare name, or a one-key `enum:` map.
///
/// `Deserialize` is written by hand so that enum keys may be integers or
/// integer strings (`1` or `"1"`, as JSON requires), and so that a mistake
/// names what is accepted instead of failing as an unmatched untagged enum.
#[cfg(feature = "config")]
#[derive(Clone, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
enum GnmiValueTypeWire {
    Name(GnmiScalarName),
    Enum(GnmiEnumWire),
}

/// The scalar value type names.
#[cfg(feature = "config")]
#[derive(Clone, Copy, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
enum GnmiScalarName {
    Uint,
    Int,
    Double,
    Bool,
    String,
}

/// `{ enum: { <i64>: <string>, ... } }`.
#[cfg(feature = "config")]
#[derive(Clone, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct GnmiEnumWire {
    #[serde(rename = "enum")]
    #[cfg_attr(
        feature = "schema",
        schemars(schema_with = "crate::schema::enum_code_map")
    )]
    map: BTreeMap<i64, String>,
}

/// What a value type may be, for error messages.
#[cfg(feature = "config")]
const VALUE_TYPE_EXPECTED: &str =
    "one of uint, int, double, bool, string, or { enum: { <integer>: <label>, ... } }";

#[cfg(feature = "config")]
impl<'de> serde::Deserialize<'de> for GnmiValueTypeWire {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ValueTypeVisitor)
    }
}

#[cfg(feature = "config")]
struct ValueTypeVisitor;

#[cfg(feature = "config")]
impl<'de> serde::de::Visitor<'de> for ValueTypeVisitor {
    type Value = GnmiValueTypeWire;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a gnmi value type: {VALUE_TYPE_EXPECTED}")
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
        let name = match v {
            "uint" => GnmiScalarName::Uint,
            "int" => GnmiScalarName::Int,
            "double" => GnmiScalarName::Double,
            "bool" => GnmiScalarName::Bool,
            "string" => GnmiScalarName::String,
            other => {
                return Err(E::custom(format!(
                    "unknown gnmi value type {other:?}; expected {VALUE_TYPE_EXPECTED}"
                )))
            }
        };
        Ok(GnmiValueTypeWire::Name(name))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;
        let mut entries: Option<BTreeMap<i64, String>> = None;
        while let Some(key) = map.next_key::<String>()? {
            if key != "enum" {
                return Err(A::Error::custom(format!(
                    "unknown key {key:?} in a gnmi value type; expected {VALUE_TYPE_EXPECTED}"
                )));
            }
            if entries.is_some() {
                return Err(A::Error::custom(
                    "duplicate `enum` key in a gnmi value type",
                ));
            }
            entries = Some(map.next_value::<EnumEntries>()?.0);
        }
        entries
            .map(|map| GnmiValueTypeWire::Enum(GnmiEnumWire { map }))
            .ok_or_else(|| A::Error::custom(format!("empty map; expected {VALUE_TYPE_EXPECTED}")))
    }
}

/// The body of `enum:`: keys are integers, written bare or as strings.
#[cfg(feature = "config")]
struct EnumEntries(BTreeMap<i64, String>);

#[cfg(feature = "config")]
impl<'de> serde::Deserialize<'de> for EnumEntries {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntriesVisitor;
        impl<'de> serde::de::Visitor<'de> for EntriesVisitor {
            type Value = EnumEntries;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a map from integer codes to labels, e.g. { 1: UP, 2: DOWN }")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut out = BTreeMap::new();
                while let Some(EnumCode(code)) = map.next_key()? {
                    let crate::config::scalar_text::ScalarText(label) = map.next_value()?;
                    if out.insert(code, label).is_some() {
                        return Err(serde::de::Error::custom(format!(
                            "enum code {code} is given more than once (keys such as 1 and \
                             \"01\" are the same code)"
                        )));
                    }
                }
                Ok(EnumEntries(out))
            }
        }
        deserializer.deserialize_map(EntriesVisitor)
    }
}

/// One `enum:` key: an integer, or a string holding one.
#[cfg(feature = "config")]
struct EnumCode(i64);

#[cfg(feature = "config")]
impl<'de> serde::Deserialize<'de> for EnumCode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct CodeVisitor;
        impl serde::de::Visitor<'_> for CodeVisitor {
            type Value = EnumCode;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an integer enum code")
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(EnumCode(v))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                i64::try_from(v)
                    .map(EnumCode)
                    .map_err(|_| E::custom(format!("enum code {v} does not fit in i64")))
            }
            /// Accepts digits with an optional leading `-` (no `+`, spaces or
            /// decimal point) that fit an `i64`. The schema's key pattern is
            /// narrower, refusing a leading zero too, so every key it accepts
            /// parses but a 19-digit code outside the `i64` range.
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                let digits = v.strip_prefix('-').unwrap_or(v);
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(E::custom(format!(
                        "enum key {v:?} is not an integer: write digits with an optional \
                         leading '-'"
                    )));
                }
                v.parse::<i64>()
                    .map(EnumCode)
                    .map_err(|_| E::custom(format!("enum code {v:?} does not fit in i64")))
            }
        }
        deserializer.deserialize_any(CodeVisitor)
    }
}

#[cfg(feature = "config")]
impl From<GnmiValueTypeWire> for GnmiValueType {
    fn from(wire: GnmiValueTypeWire) -> Self {
        match wire {
            GnmiValueTypeWire::Name(GnmiScalarName::Uint) => Self::Uint,
            GnmiValueTypeWire::Name(GnmiScalarName::Int) => Self::Int,
            GnmiValueTypeWire::Name(GnmiScalarName::Double) => Self::Double,
            GnmiValueTypeWire::Name(GnmiScalarName::Bool) => Self::Bool,
            GnmiValueTypeWire::Name(GnmiScalarName::String) => Self::String,
            GnmiValueTypeWire::Enum(e) => Self::Enum(e.map),
        }
    }
}

#[cfg(feature = "config")]
impl From<GnmiValueType> for GnmiValueTypeWire {
    fn from(value: GnmiValueType) -> Self {
        match value {
            GnmiValueType::Uint => Self::Name(GnmiScalarName::Uint),
            GnmiValueType::Int => Self::Name(GnmiScalarName::Int),
            GnmiValueType::Double => Self::Name(GnmiScalarName::Double),
            GnmiValueType::Bool => Self::Name(GnmiScalarName::Bool),
            GnmiValueType::String => Self::Name(GnmiScalarName::String),
            GnmiValueType::Enum(map) => Self::Enum(GnmiEnumWire { map }),
        }
    }
}
