//! The SNMP value type: which BER type a metric's value is encoded as, and
//! its YAML shape; and the label column, which serves a label's text as a
//! value type.
//!
//! Compiled with or without the `snmp` feature: a pack's `snmp:` hints and
//! `snmp_label_columns:` carry them. Writing a value needs the feature.

use std::collections::BTreeMap;
use std::fmt;

/// The BER type a metric's value is encoded as.
///
/// YAML forms: `integer`, `counter32`, `counter64`, `gauge32`, `unsigned32`,
/// `timeticks`, `string`, `opaque_float`, `opaque_double`, `ipaddress`, and
/// `{ enum: { 1: up, 2: down } }`. The default is `gauge32`.
///
/// The integer types, `ipaddress` and the enum round the value to the
/// nearest integer, halves away from zero. Every type but `string` rejects
/// the event when the value is not finite; the integer types and
/// `ipaddress` when the rounded value is outside the type's range,
/// `opaque_float` when the value is beyond the `f32` range, and the enum
/// when the rounded value has no entry. A counter does not wrap: a value
/// past its range is rejected, not reduced.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "config",
    serde(from = "SnmpValueTypeWire", into = "SnmpValueTypeWire")
)]
#[non_exhaustive]
pub enum SnmpValueType {
    /// INTEGER (Integer32): -2³¹ to 2³¹ − 1.
    Integer,
    /// Counter32: 0 to 2³² − 1.
    Counter32,
    /// Counter64: 0 to 2⁶⁴ − 1.
    Counter64,
    /// Gauge32: 0 to 2³² − 1.
    #[default]
    Gauge32,
    /// Unsigned32: 0 to 2³² − 1, encoded as a Gauge32.
    Unsigned32,
    /// TimeTicks, hundredths of a second: 0 to 2³² − 1.
    TimeTicks,
    /// OCTET STRING: the shortest decimal text that round-trips the `f64`.
    String,
    /// Opaque wrapping an IEEE 754 single, as net-snmp encodes it: the
    /// value converted to `f32`. A value whose conversion is not finite
    /// (beyond ±3.4 × 10³⁸) rejects the event.
    OpaqueFloat,
    /// Opaque wrapping an IEEE 754 double, as net-snmp encodes it: the
    /// value exactly.
    OpaqueDouble,
    /// IpAddress: the rounded value, 0 to 2³² − 1, as four octets, most
    /// significant first (167772161 is 10.0.0.1).
    IpAddress,
    /// OCTET STRING holding the label of the rounded value's entry, such as
    /// `up` for 1 under `{ enum: { 1: up, 2: down } }`. A value with no
    /// entry rejects the event.
    Enum(BTreeMap<i64, String>),
}

impl SnmpValueType {
    /// The name the type has in YAML, such as `"counter64"`; `"enum"` for
    /// an enum map.
    pub fn name(&self) -> &'static str {
        match self {
            SnmpValueType::Integer => "integer",
            SnmpValueType::Counter32 => "counter32",
            SnmpValueType::Counter64 => "counter64",
            SnmpValueType::Gauge32 => "gauge32",
            SnmpValueType::Unsigned32 => "unsigned32",
            SnmpValueType::TimeTicks => "timeticks",
            SnmpValueType::String => "string",
            SnmpValueType::OpaqueFloat => "opaque_float",
            SnmpValueType::OpaqueDouble => "opaque_double",
            SnmpValueType::IpAddress => "ipaddress",
            SnmpValueType::Enum(_) => "enum",
        }
    }
}

impl fmt::Display for SnmpValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// YAML shape of [`SnmpValueType`]: a bare name, or a one-key `enum:` map.
///
/// `Deserialize` is written by hand so that enum keys may be integers or
/// integer strings (`1` or `"1"`, as JSON requires), and so that a mistake
/// names what is accepted instead of failing as an unmatched untagged enum.
#[cfg(feature = "config")]
#[derive(Clone, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
enum SnmpValueTypeWire {
    Name(SnmpTypeName),
    Enum(SnmpEnumWire),
}

/// The value type names.
#[cfg(feature = "config")]
#[derive(Clone, Copy, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
enum SnmpTypeName {
    /// INTEGER (Integer32): -2³¹ to 2³¹ − 1.
    Integer,
    /// Counter32: 0 to 2³² − 1.
    Counter32,
    /// Counter64: 0 to 2⁶⁴ − 1.
    Counter64,
    /// Gauge32: 0 to 2³² − 1. The default.
    Gauge32,
    /// Unsigned32: 0 to 2³² − 1, encoded as a Gauge32.
    Unsigned32,
    /// TimeTicks, hundredths of a second: 0 to 2³² − 1.
    TimeTicks,
    /// OCTET STRING: the shortest decimal text that round-trips the value.
    String,
    /// Opaque float (net-snmp): the value as an IEEE 754 single.
    #[serde(rename = "opaque_float")]
    OpaqueFloat,
    /// Opaque double (net-snmp): the value as an IEEE 754 double.
    #[serde(rename = "opaque_double")]
    OpaqueDouble,
    /// IpAddress: the rounded value, 0 to 2³² − 1, as four octets.
    IpAddress,
}

/// `{ enum: { <i64>: <string>, ... } }`.
#[cfg(feature = "config")]
#[derive(Clone, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct SnmpEnumWire {
    /// The label of each integer code, served as an OCTET STRING; at least
    /// one entry.
    #[serde(rename = "enum")]
    #[cfg_attr(feature = "schema", schemars(extend("minProperties" = 1)))]
    map: BTreeMap<i64, String>,
}

/// What a value type may be, for error messages.
#[cfg(feature = "config")]
const VALUE_TYPE_EXPECTED: &str = "one of integer, counter32, counter64, gauge32, unsigned32, \
     timeticks, string, opaque_float, opaque_double, ipaddress, or \
     { enum: { <integer>: <label>, ... } }";

/// What an enum map may be, for error messages.
#[cfg(feature = "config")]
const ENUM_EXPECTED: &str = "{ enum: { <integer>: <label>, ... } } with at least one entry";

#[cfg(feature = "config")]
impl<'de> serde::Deserialize<'de> for SnmpValueTypeWire {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ValueTypeVisitor)
    }
}

#[cfg(feature = "config")]
struct ValueTypeVisitor;

#[cfg(feature = "config")]
impl<'de> serde::de::Visitor<'de> for ValueTypeVisitor {
    type Value = SnmpValueTypeWire;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "an snmp value type: {VALUE_TYPE_EXPECTED}")
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
        let name = match v {
            "integer" => SnmpTypeName::Integer,
            "counter32" => SnmpTypeName::Counter32,
            "counter64" => SnmpTypeName::Counter64,
            "gauge32" => SnmpTypeName::Gauge32,
            "unsigned32" => SnmpTypeName::Unsigned32,
            "timeticks" => SnmpTypeName::TimeTicks,
            "string" => SnmpTypeName::String,
            "opaque_float" => SnmpTypeName::OpaqueFloat,
            "opaque_double" => SnmpTypeName::OpaqueDouble,
            "ipaddress" => SnmpTypeName::IpAddress,
            other => {
                return Err(E::custom(format!(
                    "unknown snmp value type {other:?}; expected {VALUE_TYPE_EXPECTED}"
                )))
            }
        };
        Ok(SnmpValueTypeWire::Name(name))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;
        let mut entries: Option<BTreeMap<i64, String>> = None;
        while let Some(key) = map.next_key::<String>()? {
            if key != "enum" {
                return Err(A::Error::custom(format!(
                    "unknown key {key:?} in an snmp value type; expected {VALUE_TYPE_EXPECTED}"
                )));
            }
            if entries.is_some() {
                return Err(A::Error::custom(format!(
                    "duplicate `enum` key in an snmp value type; expected {ENUM_EXPECTED}"
                )));
            }
            entries = Some(map.next_value::<EnumEntries>()?.0);
        }
        entries
            .map(|map| SnmpValueTypeWire::Enum(SnmpEnumWire { map }))
            .ok_or_else(|| A::Error::custom(format!("empty map; expected {VALUE_TYPE_EXPECTED}")))
    }
}

/// The body of `enum:`: keys are integers, written bare or as strings, and
/// there is at least one.
#[cfg(feature = "config")]
struct EnumEntries(BTreeMap<i64, String>);

#[cfg(feature = "config")]
impl<'de> serde::Deserialize<'de> for EnumEntries {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntriesVisitor;
        impl<'de> serde::de::Visitor<'de> for EntriesVisitor {
            type Value = EnumEntries;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(
                    f,
                    "a map from integer codes to labels, e.g. {{ 1: up, 2: down }}; \
                     expected {ENUM_EXPECTED}"
                )
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
                if out.is_empty() {
                    return Err(serde::de::Error::custom(format!(
                        "an snmp enum map is empty; expected {ENUM_EXPECTED}"
                    )));
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
                write!(f, "an integer enum code; expected {ENUM_EXPECTED}")
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(EnumCode(v))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                i64::try_from(v)
                    .map(EnumCode)
                    .map_err(|_| E::custom(format!("enum code {v} does not fit in i64")))
            }
            /// Accepts exactly the schema's `^-?\\d+$`: digits with an
            /// optional leading `-`, no sign, spaces or decimal point.
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                let digits = v.strip_prefix('-').unwrap_or(v);
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(E::custom(format!(
                        "enum key {v:?} is not an integer: write digits with an optional \
                         leading '-'; expected {ENUM_EXPECTED}"
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
impl From<SnmpValueTypeWire> for SnmpValueType {
    fn from(wire: SnmpValueTypeWire) -> Self {
        match wire {
            SnmpValueTypeWire::Name(SnmpTypeName::Integer) => Self::Integer,
            SnmpValueTypeWire::Name(SnmpTypeName::Counter32) => Self::Counter32,
            SnmpValueTypeWire::Name(SnmpTypeName::Counter64) => Self::Counter64,
            SnmpValueTypeWire::Name(SnmpTypeName::Gauge32) => Self::Gauge32,
            SnmpValueTypeWire::Name(SnmpTypeName::Unsigned32) => Self::Unsigned32,
            SnmpValueTypeWire::Name(SnmpTypeName::TimeTicks) => Self::TimeTicks,
            SnmpValueTypeWire::Name(SnmpTypeName::String) => Self::String,
            SnmpValueTypeWire::Name(SnmpTypeName::OpaqueFloat) => Self::OpaqueFloat,
            SnmpValueTypeWire::Name(SnmpTypeName::OpaqueDouble) => Self::OpaqueDouble,
            SnmpValueTypeWire::Name(SnmpTypeName::IpAddress) => Self::IpAddress,
            SnmpValueTypeWire::Enum(e) => Self::Enum(e.map),
        }
    }
}

#[cfg(feature = "config")]
impl From<SnmpValueType> for SnmpValueTypeWire {
    fn from(value: SnmpValueType) -> Self {
        match value {
            SnmpValueType::Integer => Self::Name(SnmpTypeName::Integer),
            SnmpValueType::Counter32 => Self::Name(SnmpTypeName::Counter32),
            SnmpValueType::Counter64 => Self::Name(SnmpTypeName::Counter64),
            SnmpValueType::Gauge32 => Self::Name(SnmpTypeName::Gauge32),
            SnmpValueType::Unsigned32 => Self::Name(SnmpTypeName::Unsigned32),
            SnmpValueType::TimeTicks => Self::Name(SnmpTypeName::TimeTicks),
            SnmpValueType::String => Self::Name(SnmpTypeName::String),
            SnmpValueType::OpaqueFloat => Self::Name(SnmpTypeName::OpaqueFloat),
            SnmpValueType::OpaqueDouble => Self::Name(SnmpTypeName::OpaqueDouble),
            SnmpValueType::IpAddress => Self::Name(SnmpTypeName::IpAddress),
            SnmpValueType::Enum(map) => Self::Enum(SnmpEnumWire { map }),
        }
    }
}

/// A label whose value the agent serves as a table column: the OID template
/// of the column's instances, and the type the label's text is served as.
///
/// YAML forms: a string, the template, which serves the label's text as an
/// OCTET STRING; or `{ oid: <template>, type: <value type> }`, which parses
/// the label's text as an `f64` (`str::parse`, nothing more lenient) and
/// writes it as that type, except `string`, which serves the text itself, as
/// the bare form does. Validation refuses the enum type for a column, and a
/// typed column whose static label value its type cannot serve.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "config", serde(untagged, deny_unknown_fields))]
#[cfg_attr(feature = "schema", schemars(extend("x-sonda-no-widening" = true)))]
pub enum LabelColumn {
    /// The OID template; the label's text is served as an OCTET STRING. A
    /// string; a YAML number is an error.
    Text(String),
    /// The OID template, and the type the label's text is served as.
    Typed {
        /// The OID template of the column's instances. A string; a YAML
        /// number is an error.
        oid: String,
        /// The type the label's text is parsed and served as; any value
        /// type but the enum.
        #[cfg_attr(feature = "config", serde(rename = "type"))]
        r#type: SnmpValueType,
    },
}

/// What validation and the snmp encoder say when a label column's type is
/// the enum.
#[cfg(feature = "snmp")]
pub(crate) const ENUM_COLUMN_REFUSED: &str = "the enum type is refused for a label column: a \
     column serves the label's own text, and an enum would map a code the label does not carry";

impl LabelColumn {
    /// The OID template of the column's instances.
    pub fn oid(&self) -> &str {
        match self {
            LabelColumn::Text(oid) | LabelColumn::Typed { oid, .. } => oid,
        }
    }

    /// The type the label's text is served as; `None` for the bare form,
    /// which serves it as an OCTET STRING.
    pub fn value_type(&self) -> Option<&SnmpValueType> {
        match self {
            LabelColumn::Text(_) => None,
            LabelColumn::Typed { r#type, .. } => Some(r#type),
        }
    }

    /// Whether the column's type is the enum, which a column refuses
    /// ([`ENUM_COLUMN_REFUSED`]).
    #[cfg(feature = "snmp")]
    pub(crate) fn is_enum(&self) -> bool {
        matches!(self.value_type(), Some(SnmpValueType::Enum(_)))
    }
}

impl From<&str> for LabelColumn {
    /// The bare form: `oid` serves the label's text as an OCTET STRING.
    fn from(oid: &str) -> Self {
        LabelColumn::Text(oid.to_string())
    }
}

impl From<String> for LabelColumn {
    /// The bare form: `oid` serves the label's text as an OCTET STRING.
    fn from(oid: String) -> Self {
        LabelColumn::Text(oid)
    }
}

/// Reads a [`LabelColumn`]: a string, or `{ oid, type }`, every error naming
/// `field`, such as `label_columns.ifType`. The template is read as
/// [`OidText`](crate::config::scalar_text::OidText) reads one, so a YAML
/// number is an error that says to quote it; an unknown key, a missing
/// `oid:` or `type:`, and a key given twice are errors too.
#[cfg(feature = "config")]
pub(crate) struct LabelColumnSeed<'a> {
    /// The field as the error names it, such as `label_columns.ifType`.
    pub(crate) field: &'a str,
}

/// Read a map from labels to [`LabelColumn`]s, each value read by
/// [`LabelColumnSeed`] with `<field>.<label>` as its field, such as
/// `label_columns.ifType`.
#[cfg(feature = "config")]
pub(crate) fn deserialize_label_columns<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
    field: &'static str,
) -> Result<std::collections::HashMap<String, LabelColumn>, D::Error> {
    struct Columns(&'static str);
    impl<'de> serde::de::Visitor<'de> for Columns {
        type Value = std::collections::HashMap<String, LabelColumn>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                f,
                "a map from labels to label columns: {LABEL_COLUMN_EXPECTED}"
            )
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut columns = std::collections::HashMap::new();
            while let Some(label) = map.next_key::<String>()? {
                let field = format!("{}.{label}", self.0);
                let column = map.next_value_seed(LabelColumnSeed { field: &field })?;
                columns.insert(label, column);
            }
            Ok(columns)
        }
    }
    deserializer.deserialize_map(Columns(field))
}

/// What a label column may be, for error messages.
#[cfg(feature = "config")]
const LABEL_COLUMN_EXPECTED: &str = "an OID template string, or { oid: <OID template>, \
     type: <value type> }";

#[cfg(feature = "config")]
impl<'de> serde::de::DeserializeSeed<'de> for LabelColumnSeed<'_> {
    type Value = LabelColumn;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<LabelColumn, D::Error> {
        use crate::config::scalar_text::OidText;
        use serde::de::{Error, IntoDeserializer};

        struct ColumnVisitor<'a>(&'a str);
        impl ColumnVisitor<'_> {
            /// `value` read as an OID, which [`OidText`] refuses with its
            /// own message naming the field.
            fn not_text<'de, E: Error, V: IntoDeserializer<'de, E>>(
                &self,
                value: V,
            ) -> Result<LabelColumn, E> {
                serde::de::DeserializeSeed::deserialize(
                    OidText { field: self.0 },
                    value.into_deserializer(),
                )
                .map(LabelColumn::Text)
            }
        }
        impl<'de> serde::de::Visitor<'de> for ColumnVisitor<'_> {
            type Value = LabelColumn;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}: {LABEL_COLUMN_EXPECTED}", self.0)
            }
            fn visit_str<E: Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(LabelColumn::Text(v.to_string()))
            }
            fn visit_string<E: Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(LabelColumn::Text(v))
            }
            fn visit_bool<E: Error>(self, v: bool) -> Result<Self::Value, E> {
                self.not_text(v)
            }
            fn visit_i64<E: Error>(self, v: i64) -> Result<Self::Value, E> {
                self.not_text(v)
            }
            fn visit_u64<E: Error>(self, v: u64) -> Result<Self::Value, E> {
                self.not_text(v)
            }
            fn visit_f64<E: Error>(self, v: f64) -> Result<Self::Value, E> {
                self.not_text(v)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let field = self.0;
                let mut oid: Option<String> = None;
                let mut ty: Option<SnmpValueType> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "oid" if oid.is_none() => {
                            let named = format!("{field}.oid");
                            oid = Some(map.next_value_seed(OidText { field: &named })?);
                        }
                        "type" if ty.is_none() => {
                            ty = Some(
                                map.next_value::<SnmpValueType>()
                                    .map_err(|e| A::Error::custom(format!("{field}.type: {e}")))?,
                            );
                        }
                        "oid" | "type" => {
                            return Err(A::Error::custom(format!(
                                "{field}: `{key}` is given more than once; expected \
                                 {LABEL_COLUMN_EXPECTED}"
                            )))
                        }
                        other => {
                            return Err(A::Error::custom(format!(
                                "{field}: unknown key {other:?} in a label column; expected \
                                 {LABEL_COLUMN_EXPECTED}"
                            )))
                        }
                    }
                }
                match (oid, ty) {
                    (Some(oid), Some(r#type)) => Ok(LabelColumn::Typed { oid, r#type }),
                    (None, _) => Err(A::Error::custom(format!(
                        "{field}: a typed label column has no `oid:`; expected \
                         {LABEL_COLUMN_EXPECTED}"
                    ))),
                    (Some(_), None) => Err(A::Error::custom(format!(
                        "{field}: a typed label column has no `type:`; write the template \
                         alone to serve the label as an OCTET STRING, or expected \
                         {LABEL_COLUMN_EXPECTED}"
                    ))),
                }
            }
        }
        deserializer.deserialize_any(ColumnVisitor(self.field))
    }
}

/// Why a label's text could not be served as its column's type.
#[cfg(feature = "snmp")]
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("label `{label}` has value {text:?}, which its {ty} column cannot serve: {reason}")]
pub(crate) struct ColumnError {
    /// The label.
    pub(crate) label: String,
    /// The column type's [name](SnmpValueType::name).
    pub(crate) ty: &'static str,
    /// The label's text.
    pub(crate) text: String,
    /// Why: the text is not a number, or the type refuses the number.
    pub(crate) reason: String,
}

/// Write `label`'s text `text` as the value of its column, typed `ty`
/// ([`LabelColumn::value_type`]), into `out`.
///
/// No type, and `string`, write the text as an OCTET STRING. Any other type
/// parses the text as an `f64` with `str::parse` and writes it with
/// [`SnmpValueType::write`]; the enum writes nothing and is refused
/// ([`ENUM_COLUMN_REFUSED`]). On an error nothing is written. Validation and
/// the snmp encoder both apply this one rule: validation writes the static
/// value into a scratch buffer, the encoder writes the companion record.
#[cfg(feature = "snmp")]
pub(crate) fn write_label_text(
    ty: Option<&SnmpValueType>,
    label: &str,
    text: &str,
    out: &mut Vec<u8>,
) -> Result<(), ColumnError> {
    let refused = |ty: &SnmpValueType, reason: String| ColumnError {
        label: label.to_string(),
        ty: ty.name(),
        text: text.to_string(),
        reason,
    };
    match ty {
        None | Some(SnmpValueType::String) => {
            super::ber::write_string(out, text.as_bytes());
            Ok(())
        }
        Some(ty @ SnmpValueType::Enum(_)) => Err(refused(ty, ENUM_COLUMN_REFUSED.to_string())),
        Some(ty) => {
            let value: f64 = text
                .parse()
                .map_err(|_| refused(ty, "it is not a number".to_string()))?;
            ty.write(value, out).map_err(|e| refused(ty, e.to_string()))
        }
    }
}

/// Why a value could not be written as its type.
#[cfg(feature = "snmp")]
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub(crate) enum ValueError {
    /// The value is NaN or infinite and the type is not `string`.
    #[error("value {value} cannot be encoded as {ty}")]
    NonFinite {
        /// The type's [name](SnmpValueType::name).
        ty: &'static str,
        /// The value.
        value: f64,
    },
    /// The rounded value is outside the type's range, or an `opaque_float`
    /// value is beyond the `f32` range.
    #[error("value {value} is outside the {ty} range")]
    OutOfRange {
        /// The type's [name](SnmpValueType::name).
        ty: &'static str,
        /// The value, before rounding.
        value: f64,
    },
    /// The value's text did not fit the formatting buffer.
    #[error("value {value} could not be formatted as a string")]
    Format {
        /// The value.
        value: f64,
    },
    /// The rounded value has no entry in the enum map.
    #[error("value {value} rounds to {}, which has no entry in the enum map", value.round())]
    NoEnumEntry {
        /// The value, before rounding.
        value: f64,
    },
}

/// Upper bound on the `Display` length of an `f64`, sign included.
#[cfg(feature = "snmp")]
const F64_TEXT_CAPACITY: usize = 400;

/// The rounded values that fit an `i32`: from -2³¹ up to, not including,
/// 2³¹. Both bounds are exact `f64`s.
#[cfg(feature = "snmp")]
const I32_RANGE: std::ops::Range<f64> = -2_147_483_648.0..2_147_483_648.0;

/// The rounded values that fit a `u32`: from 0 up to, not including, 2³².
/// `-0.0`, which `-0.4` rounds to, is in range.
#[cfg(feature = "snmp")]
const U32_RANGE: std::ops::Range<f64> = 0.0..4_294_967_296.0;

/// The rounded values that fit a `u64`: from 0 up to, not including, 2⁶⁴.
#[cfg(feature = "snmp")]
const U64_RANGE: std::ops::Range<f64> = 0.0..18_446_744_073_709_551_616.0;

/// The rounded values that fit an `i64`: from -2⁶³ up to, not including,
/// 2⁶³. Both bounds are exact `f64`s.
#[cfg(feature = "snmp")]
const I64_RANGE: std::ops::Range<f64> = -9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0;

#[cfg(feature = "snmp")]
impl SnmpValueType {
    /// Write `value` as this type's BER TLV into `out`.
    ///
    /// Rounds to the nearest integer, halves away from zero, for the
    /// integer types, `ipaddress` and the enum. [`ValueError::NonFinite`]
    /// for NaN or an infinity under every type but `string`, and
    /// [`ValueError::OutOfRange`] when the rounded value is outside the
    /// `i32` range (`integer`), the `u32` range (`counter32`, `gauge32`,
    /// `unsigned32`, `timeticks`, `ipaddress`) or the `u64` range
    /// (`counter64`), or when an `opaque_float` value converts to an
    /// infinite `f32`. [`ValueError::NoEnumEntry`] when the enum map has no
    /// entry for the rounded value. `string` writes the shortest decimal
    /// text that round-trips the `f64`, and NaN and the infinities as `NaN`,
    /// `inf` and `-inf`. On an error nothing is written.
    pub(crate) fn write(&self, value: f64, out: &mut Vec<u8>) -> Result<(), ValueError> {
        use super::ber;

        let ty = self.name();
        let finite = || {
            if value.is_finite() {
                Ok(value)
            } else {
                Err(ValueError::NonFinite { ty, value })
            }
        };
        let rounded = |range: std::ops::Range<f64>| {
            let rounded = finite()?.round();
            if range.contains(&rounded) {
                Ok(rounded)
            } else {
                Err(ValueError::OutOfRange { ty, value })
            }
        };
        match self {
            SnmpValueType::Integer => ber::write_integer(out, rounded(I32_RANGE)? as i32),
            SnmpValueType::Counter32 => ber::write_counter32(out, rounded(U32_RANGE)? as u32),
            SnmpValueType::Gauge32 | SnmpValueType::Unsigned32 => {
                ber::write_gauge32(out, rounded(U32_RANGE)? as u32)
            }
            SnmpValueType::TimeTicks => ber::write_timeticks(out, rounded(U32_RANGE)? as u32),
            SnmpValueType::Counter64 => ber::write_counter64(out, rounded(U64_RANGE)? as u64),
            SnmpValueType::String => {
                let mut text = [0u8; F64_TEXT_CAPACITY];
                ber::write_string(out, format_f64(value, &mut text)?);
            }
            SnmpValueType::OpaqueFloat => {
                let single = finite()? as f32;
                if !single.is_finite() {
                    return Err(ValueError::OutOfRange { ty, value });
                }
                ber::write_opaque_float(out, single);
            }
            SnmpValueType::OpaqueDouble => ber::write_opaque_double(out, finite()?),
            SnmpValueType::IpAddress => ber::write_ipaddress(out, rounded(U32_RANGE)? as u32),
            SnmpValueType::Enum(labels) => {
                let code = finite()?.round();
                let label = I64_RANGE
                    .contains(&code)
                    .then(|| labels.get(&(code as i64)))
                    .flatten()
                    .ok_or(ValueError::NoEnumEntry { value })?;
                ber::write_string(out, label.as_bytes());
            }
        }
        Ok(())
    }
}

/// Format `value` with `Display` (the shortest round-trip decimal) into
/// `text` without allocating.
#[cfg(feature = "snmp")]
fn format_f64(value: f64, text: &mut [u8; F64_TEXT_CAPACITY]) -> Result<&[u8], ValueError> {
    use std::io::Write as _;
    let mut cursor = &mut text[..];
    write!(cursor, "{value}").map_err(|_| ValueError::Format { value })?;
    let written = F64_TEXT_CAPACITY - cursor.len();
    Ok(&text[..written])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(feature = "config", feature = "snmp"))]
    use rstest::rstest;

    /// `{ 1: up, 2: down }`.
    #[cfg(any(feature = "config", feature = "snmp"))]
    fn up_down() -> SnmpValueType {
        SnmpValueType::Enum(BTreeMap::from([
            (1, "up".to_string()),
            (2, "down".to_string()),
        ]))
    }

    /// A column writes the label's text verbatim for no type and `string`,
    /// and otherwise `str::parse`s it and writes it as its type; a refusal
    /// names the label, the type and the text, and writes nothing.
    #[cfg(feature = "snmp")]
    #[rustfmt::skip]
    #[rstest]
    #[case::bare_text(            None,                          "Gi0",  Ok("0403476930"))]
    #[case::string_keeps_the_text(Some(SnmpValueType::String),   "06",   Ok("04023036"))]
    #[case::integer(              Some(SnmpValueType::Integer),  "6",    Ok("020106"))]
    #[case::integer_rounds(       Some(SnmpValueType::Integer),  "6.5",  Ok("020107"))]
    #[case::gauge32_exponent(     Some(SnmpValueType::Gauge32),  "1e3",  Ok("420203e8"))]
    #[case::empty_text(           Some(SnmpValueType::Integer),  "",     Err("label `l` has value \"\", which its integer column cannot serve: it is not a number"))]
    #[case::text_is_not_trimmed(  Some(SnmpValueType::Integer),  " 6",   Err("it is not a number"))]
    #[case::outside_the_type(     Some(SnmpValueType::Counter64), "-1",  Err("which its counter64 column cannot serve: value -1 is outside the counter64 range"))]
    #[case::enum_refused(         Some(up_down()),               "1",    Err("which its enum column cannot serve: the enum type is refused for a label column"))]
    fn write_label_text_is_the_column_rule(
        #[case] ty: Option<SnmpValueType>,
        #[case] text: &str,
        #[case] expected: Result<&str, &str>,
    ) {
        let mut out = vec![0xAA];
        let written = write_label_text(ty.as_ref(), "l", text, &mut out);
        match expected {
            Ok(tlv) => {
                written.expect("served");
                let tlv: Vec<u8> = (0..tlv.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&tlv[i..i + 2], 16).expect("hex digits"))
                    .collect();
                assert_eq!(&out[1..], &tlv[..]);
            }
            Err(fragment) => {
                let shown = written.expect_err("refused").to_string();
                assert!(shown.contains(fragment), "{fragment:?} in: {shown}");
                assert_eq!(out, [0xAA], "a refusal writes nothing");
            }
        }
    }

    #[test]
    fn the_default_is_gauge32() {
        assert_eq!(SnmpValueType::default(), SnmpValueType::Gauge32);
    }

    #[cfg(feature = "config")]
    #[rustfmt::skip]
    #[rstest]
    #[case::integer(       "integer",       SnmpValueType::Integer)]
    #[case::counter32(     "counter32",     SnmpValueType::Counter32)]
    #[case::counter64(     "counter64",     SnmpValueType::Counter64)]
    #[case::gauge32(       "gauge32",       SnmpValueType::Gauge32)]
    #[case::unsigned32(    "unsigned32",    SnmpValueType::Unsigned32)]
    #[case::timeticks(     "timeticks",     SnmpValueType::TimeTicks)]
    #[case::string(        "string",        SnmpValueType::String)]
    #[case::opaque_float(  "opaque_float",  SnmpValueType::OpaqueFloat)]
    #[case::opaque_double( "opaque_double", SnmpValueType::OpaqueDouble)]
    #[case::ipaddress(     "ipaddress",     SnmpValueType::IpAddress)]
    fn names_parse_from_yaml_and_serialize_back(#[case] yaml: &str, #[case] ty: SnmpValueType) {
        let parsed: SnmpValueType = serde_yaml_ng::from_str(yaml).expect("a value type name parses");
        assert_eq!(parsed, ty);
        assert_eq!(ty.name(), yaml);
        assert_eq!(ty.to_string(), yaml);
        assert_eq!(serde_yaml_ng::to_string(&ty).expect("serializes").trim(), yaml);
    }

    #[cfg(feature = "config")]
    #[test]
    fn an_unknown_name_says_what_is_accepted() {
        let err = serde_yaml_ng::from_str::<SnmpValueType>("time_ticks").expect_err("not a type");
        let shown = err.to_string();
        assert!(
            shown.contains("time_ticks") && shown.contains("timeticks"),
            "{shown}"
        );
    }

    /// Keys bare or as integer strings; labels any scalar, kept as text.
    #[cfg(feature = "config")]
    #[rustfmt::skip]
    #[rstest]
    #[case::bare_keys(          "enum: { 1: up, 2: down }",     &[(1, "up"), (2, "down")])]
    #[case::string_key(         "enum: { \"1\": up }",          &[(1, "up")])]
    #[case::negative_key(       "enum: { \"-1\": unknown }",    &[(-1, "unknown")])]
    #[case::scalar_labels(      "enum: { 1: 200, 2: true }",    &[(1, "200"), (2, "true")])]
    fn an_enum_map_parses(#[case] yaml: &str, #[case] entries: &[(i64, &str)]) {
        let parsed: SnmpValueType = serde_yaml_ng::from_str(yaml).expect("an enum map parses");
        let expected = entries.iter().map(|(code, label)| (*code, label.to_string())).collect();
        assert_eq!(parsed, SnmpValueType::Enum(expected));
        assert_eq!(parsed.name(), "enum");
    }

    #[cfg(feature = "config")]
    #[test]
    fn an_enum_map_serializes_back() {
        let shown = serde_yaml_ng::to_string(&up_down()).expect("serializes");
        let parsed: SnmpValueType = serde_yaml_ng::from_str(&shown).expect("parses back");
        assert_eq!(parsed, up_down(), "{shown}");
    }

    /// Each refusal names the accepted shape.
    #[cfg(feature = "config")]
    #[rustfmt::skip]
    #[rstest]
    #[case::empty_enum(       "enum: {}",                   "empty")]
    #[case::non_integer_key(  "enum: { a: up }",            "\"a\" is not an integer")]
    #[case::extra_key(        "{ enum: { 1: up }, extra: 1 }", "unknown key \"extra\"")]
    #[case::misspelt_enum(    "enumm: { 1: up }",           "unknown key \"enumm\"")]
    #[case::empty_map(        "{}",                         "empty map")]
    #[case::duplicate_code(   "enum: { 1: up, \"01\": on }", "more than once")]
    fn a_malformed_enum_map_is_refused(#[case] yaml: &str, #[case] reason: &str) {
        let err = serde_yaml_ng::from_str::<SnmpValueType>(yaml).expect_err("refused");
        let shown = err.to_string();
        assert!(shown.contains(reason), "{shown}");
        if reason != "more than once" {
            assert!(shown.contains("{ enum: { <integer>: <label>, ... } }"), "names the shape: {shown}");
        }
    }

    #[cfg(feature = "snmp")]
    fn written(ty: &SnmpValueType, value: f64) -> Result<Vec<u8>, ValueError> {
        let mut out = vec![0xEE];
        let result = ty.write(value, &mut out);
        match result {
            Ok(()) => Ok(out[1..].to_vec()),
            Err(e) => {
                assert_eq!(out, [0xEE], "an error writes nothing");
                Err(e)
            }
        }
    }

    /// Expected bytes as net-snmp 5.9.4 encodes the same values (the
    /// `NET_SNMP_TRAP`, `NET_SNMP_TRAP_TYPES` and `NET_SNMP_SET_TYPES`
    /// fixtures in `ber.rs`).
    #[cfg(feature = "snmp")]
    #[rustfmt::skip]
    #[rstest]
    #[case::integer_i32_max(        SnmpValueType::Integer,      2_147_483_647.0,  "02047fffffff")]
    #[case::integer_i32_min(        SnmpValueType::Integer,      -2_147_483_648.0, "020480000000")]
    #[case::integer_rounds_up(      SnmpValueType::Integer,      1.5,              "020102")]
    #[case::integer_rounds_away(    SnmpValueType::Integer,      -128.5,           "0202ff7f")]
    #[case::counter32_u32_max(      SnmpValueType::Counter32,    4_294_967_295.0,  "410500ffffffff")]
    #[case::counter32_minus_zero(   SnmpValueType::Counter32,    -0.4,             "410100")]
    #[case::gauge32_ten_thousand(   SnmpValueType::Gauge32,      10_000.0,         "42022710")]
    #[case::unsigned32_is_gauge32(  SnmpValueType::Unsigned32,   10_000.0,         "42022710")]
    #[case::timeticks_u32_max(      SnmpValueType::TimeTicks,    4_294_967_295.0,  "430500ffffffff")]
    #[case::counter64_two_pow_63(   SnmpValueType::Counter64,    9_223_372_036_854_775_808.0, "4609008000000000000000")]
    #[case::string_shortest(        SnmpValueType::String,       12.5,             "040431322e35")]
    #[case::string_nan(             SnmpValueType::String,       f64::NAN,         "04034e614e")]
    #[case::opaque_float(           SnmpValueType::OpaqueFloat,  0.15,             "44079f78043e19999a")]
    #[case::opaque_double(          SnmpValueType::OpaqueDouble, 0.15,             "440b9f79083fc3333333333333")]
    #[case::ipaddress_ten_0_0_1(    SnmpValueType::IpAddress,    167_772_161.0,    "40040a000001")]
    #[case::ipaddress_u32_max(      SnmpValueType::IpAddress,    4_294_967_295.0,  "4004ffffffff")]
    #[case::ipaddress_rounds_away(  SnmpValueType::IpAddress,    167_772_160.5,    "40040a000001")]
    #[case::enum_up(                up_down(),                   1.0,              "04027570")]
    #[case::enum_half_rounds_away(  up_down(),                   1.5,              "0404646f776e")]
    #[case::enum_half_rounds_up(    up_down(),                   0.5,              "04027570")]
    fn writes_the_typed_tlv(#[case] ty: SnmpValueType, #[case] value: f64, #[case] expected: &str) {
        let expected: Vec<u8> = (0..expected.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&expected[i..i + 2], 16).expect("hex"))
            .collect();
        assert_eq!(written(&ty, value).expect("in range"), expected, "{ty} {value}");
    }

    #[cfg(feature = "snmp")]
    #[test]
    fn counter64_reaches_its_largest_f64_below_two_pow_64() {
        // 2^64 - 2048 is the largest f64 below 2^64.
        let largest = 18_446_744_073_709_549_568.0;
        let mut expected = vec![0x46, 0x09, 0x00];
        expected.extend_from_slice(&(largest as u64).to_be_bytes());
        assert_eq!(
            written(&SnmpValueType::Counter64, largest).expect("in range"),
            expected
        );
    }

    /// Any finite value is written exactly, the extremes included.
    #[cfg(feature = "snmp")]
    #[rstest]
    fn opaque_double_writes_every_finite_value_exactly(
        #[values(f64::MAX, f64::MIN, f64::MIN_POSITIVE, 5e-324, -0.0, 0.1)] value: f64,
    ) {
        let bytes = written(&SnmpValueType::OpaqueDouble, value).expect("finite");
        assert_eq!(&bytes[..5], &[0x44, 0x0B, 0x9F, 0x79, 0x08]);
        let back = f64::from_be_bytes(bytes[5..].try_into().expect("eight octets"));
        assert_eq!(back.to_bits(), value.to_bits());
    }

    #[cfg(feature = "snmp")]
    #[rustfmt::skip]
    #[rstest]
    #[case::integer_i32_max_plus_one( SnmpValueType::Integer,    2_147_483_648.0)]
    #[case::integer_rounds_past_max(  SnmpValueType::Integer,    2_147_483_647.5)]
    #[case::integer_below_min(        SnmpValueType::Integer,    -2_147_483_649.0)]
    #[case::counter32_two_pow_32(     SnmpValueType::Counter32,  4_294_967_296.0)]
    #[case::counter32_negative(       SnmpValueType::Counter32,  -0.5)]
    #[case::gauge32_two_pow_32(       SnmpValueType::Gauge32,    4_294_967_296.0)]
    #[case::unsigned32_two_pow_32(    SnmpValueType::Unsigned32, 4_294_967_296.0)]
    #[case::timeticks_two_pow_32(     SnmpValueType::TimeTicks,  4_294_967_296.0)]
    #[case::counter64_two_pow_64(     SnmpValueType::Counter64,  18_446_744_073_709_551_616.0)]
    #[case::counter64_negative(       SnmpValueType::Counter64,  -1.0)]
    #[case::ipaddress_negative(       SnmpValueType::IpAddress,  -1.0)]
    #[case::ipaddress_two_pow_32(     SnmpValueType::IpAddress,  4_294_967_296.0)]
    #[case::opaque_float_beyond_f32(  SnmpValueType::OpaqueFloat, 1e39)]
    #[case::opaque_float_below_f32(   SnmpValueType::OpaqueFloat, -1e39)]
    fn out_of_range_is_rejected(#[case] ty: SnmpValueType, #[case] value: f64) {
        assert_eq!(
            written(&ty, value),
            Err(ValueError::OutOfRange { ty: ty.name(), value })
        );
    }

    /// A value whose rounded code is not a key, or is past the `i64` range
    /// (which a saturating conversion would read as `i64::MAX`), has no
    /// entry; nothing is written.
    #[cfg(feature = "snmp")]
    #[rstest]
    #[case::no_code_three(up_down(), 3.0)]
    #[case::rounds_to_zero(up_down(), 0.4)]
    #[case::past_i64(SnmpValueType::Enum(BTreeMap::from([(i64::MAX, "max".to_string())])), 1e300)]
    fn an_enum_value_without_an_entry_is_rejected(#[case] ty: SnmpValueType, #[case] value: f64) {
        assert_eq!(written(&ty, value), Err(ValueError::NoEnumEntry { value }));
    }

    #[cfg(feature = "snmp")]
    #[test]
    fn the_smallest_enum_code_is_reached() {
        let ty = SnmpValueType::Enum(BTreeMap::from([(i64::MIN, "min".to_string())]));
        assert_eq!(
            written(&ty, -9_223_372_036_854_775_808.0),
            Ok(b"\x04\x03min".to_vec())
        );
    }

    #[cfg(feature = "snmp")]
    #[rstest]
    fn non_finite_is_rejected_by_every_type_but_string(
        #[values(
            SnmpValueType::Integer,
            SnmpValueType::Counter32,
            SnmpValueType::Counter64,
            SnmpValueType::Gauge32,
            SnmpValueType::Unsigned32,
            SnmpValueType::TimeTicks,
            SnmpValueType::OpaqueFloat,
            SnmpValueType::OpaqueDouble,
            SnmpValueType::IpAddress,
            up_down()
        )]
        ty: SnmpValueType,
        #[values(f64::NAN, f64::INFINITY, f64::NEG_INFINITY)] value: f64,
    ) {
        assert!(matches!(
            written(&ty, value),
            Err(ValueError::NonFinite { ty: name, .. }) if name == ty.name()
        ));
    }

    #[cfg(feature = "snmp")]
    #[rstest]
    #[case::nan(f64::NAN, "NaN")]
    #[case::infinity(f64::INFINITY, "inf")]
    #[case::negative_infinity(f64::NEG_INFINITY, "-inf")]
    fn string_writes_non_finite_values_as_text(#[case] value: f64, #[case] text: &str) {
        let mut expected = vec![0x04, text.len() as u8];
        expected.extend_from_slice(text.as_bytes());
        assert_eq!(written(&SnmpValueType::String, value), Ok(expected));
    }

    #[cfg(feature = "snmp")]
    #[test]
    fn extreme_values_format_within_the_stack_buffer() {
        for value in [
            f64::MAX,
            f64::MIN,
            f64::MIN_POSITIVE,
            -f64::MIN_POSITIVE,
            5e-324,
        ] {
            let bytes = written(&SnmpValueType::String, value).expect("formats");
            // Short form below 128 octets of text, long form above.
            let header = if bytes[1] & 0x80 == 0 {
                2
            } else {
                2 + usize::from(bytes[1] & 0x7F)
            };
            assert!(bytes.len() > 128 || header == 2);
            let text = std::str::from_utf8(&bytes[header..]).expect("ascii");
            assert_eq!(text.parse::<f64>().expect("round-trips"), value);
        }
    }
}
