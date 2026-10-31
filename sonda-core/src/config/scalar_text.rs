//! Deserialization of text fields that take any scalar.
//!
//! A string, number or boolean is read as its display text: `57400` becomes
//! `"57400"`, `false` becomes `"false"`, `1.5` becomes `"1.5"`, and a float
//! with no fractional part, such as `1.0`, becomes `"1"`. The generated
//! schema accepts any scalar for a text field, so every value it accepts
//! parses, and the field's own validation reports what is wrong with it.
//!
//! An OID is the exception: read as a number, `1.30` is `1.3`. [`OidText`]
//! reads a string only, and its schema position is not widened.

use std::fmt;

use serde::de::{DeserializeSeed, Deserializer, Error, Visitor};
use serde::Deserialize;

/// A scalar read as its display text.
pub(crate) struct ScalarText(pub(crate) String);

impl<'de> Deserialize<'de> for ScalarText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TextVisitor;
        impl Visitor<'_> for TextVisitor {
            type Value = ScalarText;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string, number or boolean")
            }
            fn visit_str<E: Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(ScalarText(v.to_string()))
            }
            fn visit_string<E: Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(ScalarText(v))
            }
            fn visit_bool<E: Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(ScalarText(v.to_string()))
            }
            fn visit_i64<E: Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(ScalarText(v.to_string()))
            }
            fn visit_u64<E: Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(ScalarText(v.to_string()))
            }
            fn visit_f64<E: Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(ScalarText(v.to_string()))
            }
        }
        deserializer.deserialize_any(TextVisitor)
    }
}

/// `deserialize_with` for a required text field.
#[cfg(any(feature = "gnmi", feature = "snmp"))]
pub(crate) fn required<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    ScalarText::deserialize(deserializer).map(|text| text.0)
}

/// `deserialize_with` for an optional text field; `null` is `None`.
pub(crate) fn optional<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<ScalarText>::deserialize(deserializer).map(|text| text.map(|text| text.0))
}

/// Reads an OID or OID template field as a string only.
///
/// A YAML number or boolean is an error that names `field` and says to
/// quote the value. It reads through `deserialize_any`, the path on which an
/// unquoted `1.30` arrives as the number it resolves to rather than as its
/// text; an unquoted dotted OID such as `1.3.6.1.2.1.1.3.0` is a YAML string
/// and reads as written.
pub(crate) struct OidText<'a> {
    /// The field as the error names it, such as `oids.ifHCInOctets`.
    pub(crate) field: &'a str,
}

impl<'de> DeserializeSeed<'de> for OidText<'_> {
    type Value = String;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<String, D::Error> {
        struct OidVisitor<'a>(&'a str);
        impl OidVisitor<'_> {
            fn not_text<E: Error>(&self, kind: &str, value: &dyn fmt::Display) -> E {
                E::custom(format!(
                    "{}: an OID is written as a string, and the YAML {kind} {value} is not \
                     one; quote it, as in \"1.3.6.1.2.1.2.2.1.8.{{ifIndex}}\"",
                    self.0
                ))
            }
        }
        impl Visitor<'_> for OidVisitor<'_> {
            type Value = String;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "an OID string for {}", self.0)
            }
            fn visit_str<E: Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(v.to_string())
            }
            fn visit_string<E: Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(v)
            }
            fn visit_bool<E: Error>(self, v: bool) -> Result<Self::Value, E> {
                Err(self.not_text("boolean", &v))
            }
            fn visit_i64<E: Error>(self, v: i64) -> Result<Self::Value, E> {
                Err(self.not_text("number", &v))
            }
            fn visit_u64<E: Error>(self, v: u64) -> Result<Self::Value, E> {
                Err(self.not_text("number", &v))
            }
            fn visit_f64<E: Error>(self, v: f64) -> Result<Self::Value, E> {
                Err(self.not_text("number", &v))
            }
        }
        deserializer.deserialize_any(OidVisitor(self.field))
    }
}
