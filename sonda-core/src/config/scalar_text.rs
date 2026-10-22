//! Deserialization of text fields that take any scalar.
//!
//! A string, number or boolean is read as its display text: `57400` becomes
//! `"57400"`, `false` becomes `"false"`, `1.5` becomes `"1.5"`, and a float
//! with no fractional part, such as `1.0`, becomes `"1"`. The generated
//! schema accepts any scalar for a text field, so every value it accepts
//! parses, and the field's own validation reports what is wrong with it.

use std::fmt;

use serde::de::{Deserializer, Error, Visitor};
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
#[cfg(feature = "gnmi")]
pub(crate) fn required<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    ScalarText::deserialize(deserializer).map(|text| text.0)
}

/// `deserialize_with` for an optional text field; `null` is `None`.
#[cfg(feature = "gnmi")]
pub(crate) fn optional<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<ScalarText>::deserialize(deserializer).map(|text| text.map(|text| text.0))
}
