//! The SNMP value type: which BER type a metric's value is encoded as, and
//! its YAML shape.
//!
//! Compiled with or without the `snmp` feature: a pack's `snmp:` hints
//! carry it. Writing a value needs the feature.

use std::fmt;

/// The BER type a metric's value is encoded as.
///
/// YAML forms: `integer`, `counter32`, `counter64`, `gauge32`, `unsigned32`,
/// `timeticks`, `string`. The default is `gauge32`.
///
/// Every type but `string` rounds the value to the nearest integer, halves
/// away from zero, and rejects the event when the value is not finite or
/// the rounded value is outside the type's range. A counter does not wrap:
/// a value past its range is rejected, not reduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "config", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config", serde(rename_all = "lowercase"))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
}

impl SnmpValueType {
    /// The name the type has in YAML, such as `"counter64"`.
    pub fn name(self) -> &'static str {
        match self {
            SnmpValueType::Integer => "integer",
            SnmpValueType::Counter32 => "counter32",
            SnmpValueType::Counter64 => "counter64",
            SnmpValueType::Gauge32 => "gauge32",
            SnmpValueType::Unsigned32 => "unsigned32",
            SnmpValueType::TimeTicks => "timeticks",
            SnmpValueType::String => "string",
        }
    }
}

impl fmt::Display for SnmpValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why a value could not be written as its type.
#[cfg(feature = "snmp")]
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub(crate) enum ValueError {
    /// The value is NaN or infinite and the type is an integer type.
    #[error("value {value} cannot be encoded as {ty}")]
    NonFinite {
        /// The type.
        ty: SnmpValueType,
        /// The value.
        value: f64,
    },
    /// The rounded value is outside the type's range.
    #[error("value {value} is outside the {ty} range")]
    OutOfRange {
        /// The type.
        ty: SnmpValueType,
        /// The value, before rounding.
        value: f64,
    },
    /// The value's text did not fit the formatting buffer.
    #[error("value {value} could not be formatted as a string")]
    Format {
        /// The value.
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

#[cfg(feature = "snmp")]
impl SnmpValueType {
    /// Write `value` as this type's BER TLV into `out`.
    ///
    /// Rounds to the nearest integer, halves away from zero, for every type
    /// but `string`. [`ValueError::NonFinite`] for NaN or an infinity, and
    /// [`ValueError::OutOfRange`] when the rounded value is outside the
    /// `i32` range (`integer`), the `u32` range (`counter32`, `gauge32`,
    /// `unsigned32`, `timeticks`) or the `u64` range (`counter64`). `string`
    /// writes the shortest decimal text that round-trips the `f64`, and NaN
    /// and the infinities as `NaN`, `inf` and `-inf`. On an error nothing is
    /// written.
    pub(crate) fn write(self, value: f64, out: &mut Vec<u8>) -> Result<(), ValueError> {
        use super::ber;

        let rounded = |range: std::ops::Range<f64>| {
            if !value.is_finite() {
                return Err(ValueError::NonFinite { ty: self, value });
            }
            let rounded = value.round();
            if range.contains(&rounded) {
                Ok(rounded)
            } else {
                Err(ValueError::OutOfRange { ty: self, value })
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

    #[test]
    fn the_default_is_gauge32() {
        assert_eq!(SnmpValueType::default(), SnmpValueType::Gauge32);
    }

    #[cfg(feature = "config")]
    #[rustfmt::skip]
    #[rstest]
    #[case::integer(    "integer",    SnmpValueType::Integer)]
    #[case::counter32(  "counter32",  SnmpValueType::Counter32)]
    #[case::counter64(  "counter64",  SnmpValueType::Counter64)]
    #[case::gauge32(    "gauge32",    SnmpValueType::Gauge32)]
    #[case::unsigned32( "unsigned32", SnmpValueType::Unsigned32)]
    #[case::timeticks(  "timeticks",  SnmpValueType::TimeTicks)]
    #[case::string(     "string",     SnmpValueType::String)]
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

    #[cfg(feature = "snmp")]
    fn written(ty: SnmpValueType, value: f64) -> Result<Vec<u8>, ValueError> {
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
    /// `NET_SNMP_TRAP` and `NET_SNMP_TRAP_TYPES` fixtures in `ber.rs`).
    #[cfg(feature = "snmp")]
    #[rustfmt::skip]
    #[rstest]
    #[case::integer_i32_max(        SnmpValueType::Integer,    2_147_483_647.0,  "02047fffffff")]
    #[case::integer_i32_min(        SnmpValueType::Integer,    -2_147_483_648.0, "020480000000")]
    #[case::integer_rounds_up(      SnmpValueType::Integer,    1.5,              "020102")]
    #[case::integer_rounds_away(    SnmpValueType::Integer,    -128.5,           "0202ff7f")]
    #[case::counter32_u32_max(      SnmpValueType::Counter32,  4_294_967_295.0,  "410500ffffffff")]
    #[case::counter32_minus_zero(   SnmpValueType::Counter32,  -0.4,             "410100")]
    #[case::gauge32_ten_thousand(   SnmpValueType::Gauge32,    10_000.0,         "42022710")]
    #[case::unsigned32_is_gauge32(  SnmpValueType::Unsigned32, 10_000.0,         "42022710")]
    #[case::timeticks_u32_max(      SnmpValueType::TimeTicks,  4_294_967_295.0,  "430500ffffffff")]
    #[case::counter64_two_pow_63(   SnmpValueType::Counter64,  9_223_372_036_854_775_808.0, "4609008000000000000000")]
    #[case::string_shortest(        SnmpValueType::String,     12.5,             "040431322e35")]
    #[case::string_nan(             SnmpValueType::String,     f64::NAN,         "04034e614e")]
    fn writes_the_typed_tlv(#[case] ty: SnmpValueType, #[case] value: f64, #[case] expected: &str) {
        let expected: Vec<u8> = (0..expected.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&expected[i..i + 2], 16).expect("hex"))
            .collect();
        assert_eq!(written(ty, value).expect("in range"), expected, "{ty} {value}");
    }

    #[cfg(feature = "snmp")]
    #[test]
    fn counter64_reaches_its_largest_f64_below_two_pow_64() {
        // 2^64 - 2048 is the largest f64 below 2^64.
        let largest = 18_446_744_073_709_549_568.0;
        let mut expected = vec![0x46, 0x09, 0x00];
        expected.extend_from_slice(&(largest as u64).to_be_bytes());
        assert_eq!(
            written(SnmpValueType::Counter64, largest).expect("in range"),
            expected
        );
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
    fn out_of_range_is_rejected(#[case] ty: SnmpValueType, #[case] value: f64) {
        assert_eq!(written(ty, value), Err(ValueError::OutOfRange { ty, value }));
    }

    #[cfg(feature = "snmp")]
    #[rstest]
    fn non_finite_is_rejected_by_every_integer_type(
        #[values(
            SnmpValueType::Integer,
            SnmpValueType::Counter32,
            SnmpValueType::Counter64,
            SnmpValueType::Gauge32,
            SnmpValueType::Unsigned32,
            SnmpValueType::TimeTicks
        )]
        ty: SnmpValueType,
        #[values(f64::NAN, f64::INFINITY, f64::NEG_INFINITY)] value: f64,
    ) {
        assert!(matches!(
            written(ty, value),
            Err(ValueError::NonFinite { .. })
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
        assert_eq!(written(SnmpValueType::String, value), Ok(expected));
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
            let bytes = written(SnmpValueType::String, value).expect("formats");
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
