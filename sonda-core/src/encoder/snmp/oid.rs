//! SNMP object identifiers.
//!
//! An [`Oid`] is a sequence of unsigned 32-bit arcs. Its [`Ord`] is
//! lexicographic by arc, with a prefix ordering before every OID it is a
//! prefix of: the order `GETNEXT` and `GETBULK` walk a MIB in, so a
//! `BTreeMap<Oid, _>` range scan is a walk.
//!
//! # Text form
//!
//! Dotted decimal without a leading dot: `1.3.6.1.2.1.1.3.0`. Every arc is a
//! decimal `u32`. There are at least two arcs, the first is 0, 1 or 2, and
//! when it is 0 or 1 the second is below 40, because BER folds the first two
//! arcs into one subidentifier (X.690 §8.19.4) and nothing else can be
//! encoded.
//!
//! # BER form
//!
//! The content octets of an OBJECT IDENTIFIER: the first two arcs as one
//! subidentifier `40 * first + second`, then one subidentifier per remaining
//! arc, each base-128 big-endian with the high bit set on every octet but the
//! last.
//!
//! # Templates
//!
//! An [`OidTemplate`] is an object's OID followed by zero or more index
//! placeholders, each one whole trailing arc: `1.3.6.1.2.1.31.1.1.1.6.{ifIndex}`
//! renders the event's `ifIndex` label as the last arc. A scalar has no
//! placeholder and ends in `.0`. A label value renders as an arc only in the
//! form [`index_arc`] accepts.

use std::borrow::Borrow;
use std::fmt;

use crate::model::metric::{is_valid_label_key, Labels};

/// The most arcs an OID may have (RFC 2578 §3.5).
pub(crate) const MAX_ARCS: usize = 128;

/// An SNMP object identifier: a sequence of `u32` arcs.
///
/// Ordered lexicographically by arc, which is the order a MIB walk visits
/// instances in: `1.3.6.1.2.1.2.2.1.10.9` sorts before `…10.10`, and an OID
/// sorts before every OID it is a prefix of.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Oid(Vec<u32>);

/// Why a dotted OID string did not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OidParseError {
    /// The string starts with `.`; Sonda's OIDs are written without one.
    #[error("OID {0:?} starts with '.'; write it without the leading dot")]
    LeadingDot(String),
    /// Two dots in a row, a trailing dot, or an empty string.
    #[error("OID {0:?} has an empty arc")]
    EmptyArc(String),
    /// A single arc; BER encodes at least two.
    #[error("OID {0:?} has one arc; an OID has at least two")]
    OneArc(String),
    /// An arc that is not a decimal number fitting in 32 bits.
    #[error("OID {oid:?} has arc {arc:?}, which is not an unsigned 32-bit integer")]
    BadArc {
        /// The whole OID as written.
        oid: String,
        /// The offending arc.
        arc: String,
    },
    /// The first two arcs cannot be folded into one BER subidentifier.
    #[error(
        "OID {0:?} cannot be encoded: the first arc must be 0, 1 or 2, and the second below 40 when the first is 0 or 1"
    )]
    NotEncodable(String),
}

impl Oid {
    /// Parse a dotted OID such as `1.3.6.1.2.1.1.3.0`.
    ///
    /// Rejects a leading dot, an empty arc, an arc that is not a `u32`, a
    /// single arc, and a first-arc pair BER cannot encode.
    pub fn parse(text: &str) -> Result<Self, OidParseError> {
        if text.starts_with('.') {
            return Err(OidParseError::LeadingDot(text.to_owned()));
        }
        let mut arcs = Vec::new();
        for arc in text.split('.') {
            if arc.is_empty() {
                return Err(OidParseError::EmptyArc(text.to_owned()));
            }
            // `u32::from_str` accepts a leading `+`; an arc is digits only.
            if !arc.bytes().all(|b| b.is_ascii_digit()) {
                return Err(OidParseError::BadArc {
                    oid: text.to_owned(),
                    arc: arc.to_owned(),
                });
            }
            let value = arc.parse::<u32>().map_err(|_| OidParseError::BadArc {
                oid: text.to_owned(),
                arc: arc.to_owned(),
            })?;
            arcs.push(value);
        }
        if arcs.len() < 2 {
            return Err(OidParseError::OneArc(text.to_owned()));
        }
        if !first_arcs_encodable(&arcs) {
            return Err(OidParseError::NotEncodable(text.to_owned()));
        }
        Ok(Oid(arcs))
    }

    /// Build an OID from arcs, or `None` when there are fewer than two or
    /// the first two cannot be BER-encoded.
    pub fn from_arcs(arcs: Vec<u32>) -> Option<Self> {
        if arcs.len() < 2 || !first_arcs_encodable(&arcs) {
            return None;
        }
        Some(Oid(arcs))
    }

    /// The arcs, first to last.
    pub fn arcs(&self) -> &[u32] {
        &self.0
    }

    /// Whether `self` is a prefix of `other`; an OID is a prefix of itself.
    pub fn is_prefix_of(&self, other: &Oid) -> bool {
        other.0.starts_with(&self.0)
    }

    /// The number of content octets [`Oid::write_ber_body`] writes.
    pub(crate) fn ber_body_len(&self) -> usize {
        ber_body_len(&self.0)
    }

    /// Write the BER content octets (no tag, no length) into `out`.
    pub(crate) fn write_ber_body(&self, out: &mut Vec<u8>) {
        write_ber_body(&self.0, out);
    }

    /// Decode BER content octets into an OID.
    ///
    /// `None` on an empty body, a subidentifier left unterminated, a
    /// subidentifier with a leading `0x80` octet (not minimal, X.690
    /// §8.19.2) or an arc that does not fit in 32 bits.
    pub(crate) fn from_ber_body(body: &[u8]) -> Option<Self> {
        let mut arcs = Vec::new();
        decode_ber_body(body, &mut arcs)?;
        Some(Oid(arcs))
    }
}

impl Borrow<[u32]> for Oid {
    fn borrow(&self) -> &[u32] {
        &self.0
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut arcs = self.0.iter();
        if let Some(first) = arcs.next() {
            write!(f, "{first}")?;
        }
        for arc in arcs {
            write!(f, ".{arc}")?;
        }
        Ok(())
    }
}

/// The arc a label value renders as in an OID instance: the value read as a
/// canonical decimal `u32` (ASCII digits only, no sign, no leading zero
/// unless it is `0`), or `None`.
///
/// The one rule rendering and validation both apply. It is canonical so that
/// two distinct label values never render the same arc: `01` is refused
/// rather than read as `1`.
pub(crate) fn index_arc(value: &str) -> Option<u32> {
    let digits = !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
    if !digits || (value.len() > 1 && value.starts_with('0')) {
        return None;
    }
    value.parse().ok()
}

/// An OID template: an object's OID, then zero or more index placeholders,
/// each one whole trailing arc.
///
/// `1.3.6.1.2.1.2.2.1.8.{ifIndex}` is the object `1.3.6.1.2.1.2.2.1.8` with
/// one index arc; `1.3.6.1.2.1.31.1.1.1.6.{a}.{b}` has two;
/// `1.3.6.1.4.1.8072.9999.1.1.0` is a scalar with none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OidTemplate {
    object: Oid,
    placeholders: Vec<String>,
}

/// Why an OID template did not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum OidTemplateError {
    /// The arcs before the first placeholder are not an encodable OID.
    #[error("OID template {template:?}: {source}")]
    Object {
        /// The template as written.
        template: String,
        /// Why its object OID did not parse.
        #[source]
        source: OidParseError,
    },
    /// The template starts with a placeholder.
    #[error(
        "OID template {template:?} starts with a placeholder; it starts with the \
         object's OID, as in 1.3.6.1.2.1.2.2.1.8.{{ifIndex}}"
    )]
    NoObject {
        /// The template as written.
        template: String,
    },
    /// A placeholder in a form other than `{label}`, such as `{ifName:str}`.
    #[error(
        "OID template {template:?}: placeholder {placeholder} is not supported; an \
         index placeholder is a label whose values are unsigned integers, written \
         {{label}}"
    )]
    Unsupported {
        /// The template as written.
        template: String,
        /// The placeholder as written, braces included.
        placeholder: String,
    },
    /// A literal arc after a placeholder.
    #[error(
        "OID template {template:?}: arc {arc:?} follows a placeholder; placeholders \
         are the last arcs, after the object's OID"
    )]
    NotTrailing {
        /// The template as written.
        template: String,
        /// The arc after a placeholder.
        arc: String,
    },
    /// An arc with a brace that is not a whole `{label}`.
    #[error(
        "OID template {template:?}: arc {arc:?} is neither a number nor a placeholder \
         {{label}} naming a label key"
    )]
    BadPlaceholder {
        /// The template as written.
        template: String,
        /// The offending arc.
        arc: String,
    },
    /// More arcs than an OID may have.
    #[error("OID template {template:?} has {arcs} arcs; an OID has at most 128")]
    TooManyArcs {
        /// The template as written.
        template: String,
        /// How many arcs it has.
        arcs: usize,
    },
}

/// Why a template could not be rendered for an event.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum RenderError {
    /// The event lacks the label a placeholder names.
    #[error("the OID template's placeholder {{{0}}} names a label the event does not carry")]
    MissingLabel(String),
    /// The label's value is not an arc ([`index_arc`]).
    #[error(
        "label `{name}` has value {value:?}, which is not an OID arc (an unsigned \
         32-bit integer in decimal, without leading zeros)"
    )]
    NotAnArc {
        /// The label.
        name: String,
        /// Its value.
        value: String,
    },
}

impl OidTemplate {
    /// Parse a template such as `1.3.6.1.2.1.31.1.1.1.6.{ifIndex}`.
    ///
    /// The arcs before the first placeholder must parse as an [`Oid`] (at
    /// least two arcs, an encodable first pair). Each placeholder is a whole
    /// arc `{label}` naming a valid label key, and only placeholders follow
    /// the first one. `{label:str}`, or any other `{label:...}` form, is
    /// [`OidTemplateError::Unsupported`]. The template has at most
    /// [`MAX_ARCS`] arcs.
    pub(crate) fn parse(text: &str) -> Result<Self, OidTemplateError> {
        let mut literal: Vec<&str> = Vec::new();
        let mut placeholders: Vec<String> = Vec::new();
        for arc in text.split('.') {
            let placeholder = match arc.strip_prefix('{').and_then(|a| a.strip_suffix('}')) {
                Some(inner) if inner.contains(':') => {
                    return Err(OidTemplateError::Unsupported {
                        template: text.to_owned(),
                        placeholder: arc.to_owned(),
                    })
                }
                Some(inner) if is_valid_label_key(inner) => Some(inner),
                Some(_) => {
                    return Err(OidTemplateError::BadPlaceholder {
                        template: text.to_owned(),
                        arc: arc.to_owned(),
                    })
                }
                None if arc.contains(['{', '}']) => {
                    return Err(OidTemplateError::BadPlaceholder {
                        template: text.to_owned(),
                        arc: arc.to_owned(),
                    })
                }
                None => None,
            };
            match placeholder {
                Some(name) => placeholders.push(name.to_owned()),
                None if !placeholders.is_empty() => {
                    return Err(OidTemplateError::NotTrailing {
                        template: text.to_owned(),
                        arc: arc.to_owned(),
                    })
                }
                None => literal.push(arc),
            }
        }
        if literal.is_empty() {
            return Err(OidTemplateError::NoObject {
                template: text.to_owned(),
            });
        }
        let object = Oid::parse(&literal.join(".")).map_err(|source| OidTemplateError::Object {
            template: text.to_owned(),
            source,
        })?;
        let arcs = object.arcs().len() + placeholders.len();
        if arcs > MAX_ARCS {
            return Err(OidTemplateError::TooManyArcs {
                template: text.to_owned(),
                arcs,
            });
        }
        Ok(Self {
            object,
            placeholders,
        })
    }

    /// The labels the placeholders name, in arc order.
    pub(crate) fn placeholders(&self) -> impl Iterator<Item = &str> {
        self.placeholders.iter().map(String::as_str)
    }

    /// The object the instances belong to: the arcs before the first
    /// placeholder, or the whole OID for a template without one.
    pub(crate) fn object(&self) -> &Oid {
        &self.object
    }

    /// How many index arcs follow the object: the number of placeholders,
    /// 0 for a scalar. At most [`MAX_ARCS`] minus two, so it fits a `u8`.
    pub(crate) fn index_arcs(&self) -> u8 {
        // parse() caps the arcs at MAX_ARCS (128) and the object has two.
        self.placeholders.len() as u8
    }

    /// Write the instance OID's arcs for `labels` into `out` (cleared first):
    /// the object's arcs, then each placeholder's label value as an arc.
    ///
    /// [`RenderError::MissingLabel`] when `labels` lacks a placeholder's
    /// label, [`RenderError::NotAnArc`] when its value is not an
    /// [`index_arc`].
    pub(crate) fn render(&self, labels: &Labels, out: &mut Vec<u32>) -> Result<(), RenderError> {
        out.clear();
        out.extend_from_slice(self.object.arcs());
        for name in &self.placeholders {
            let value = labels
                .iter()
                .find_map(|(key, value)| (key == name).then_some(value))
                .ok_or_else(|| RenderError::MissingLabel(name.clone()))?;
            let arc = index_arc(value).ok_or_else(|| RenderError::NotAnArc {
                name: name.clone(),
                value: value.to_owned(),
            })?;
            out.push(arc);
        }
        Ok(())
    }
}

impl fmt::Display for OidTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.object)?;
        for name in &self.placeholders {
            write!(f, ".{{{name}}}")?;
        }
        Ok(())
    }
}

/// Whether the first two arcs fold into one BER subidentifier that fits the
/// decoder's range: first arc 0..=2, second below 40 unless the first is 2.
/// Callers have checked there are two.
fn first_arcs_encodable(arcs: &[u32]) -> bool {
    match arcs {
        [0 | 1, second, ..] => *second < 40,
        [2, second, ..] => *second <= u32::MAX - 80,
        _ => false,
    }
}

/// The first subidentifier: the first two arcs folded. An `Oid` always has
/// two arcs; a shorter slice folds the missing ones as zero.
fn first_subidentifier(arcs: &[u32]) -> u64 {
    let first = arcs.first().copied().unwrap_or(0);
    let second = arcs.get(1).copied().unwrap_or(0);
    u64::from(first) * 40 + u64::from(second)
}

/// Octets a base-128 subidentifier takes.
fn subidentifier_len(value: u64) -> usize {
    let bits = 64 - value.leading_zeros() as usize;
    bits.div_ceil(7).max(1)
}

fn write_subidentifier(value: u64, out: &mut Vec<u8>) {
    let len = subidentifier_len(value);
    for i in (0..len).rev() {
        let septet = ((value >> (7 * i)) & 0x7F) as u8;
        out.push(if i == 0 { septet } else { septet | 0x80 });
    }
}

fn ber_body_len(arcs: &[u32]) -> usize {
    let rest = arcs.get(2..).unwrap_or(&[]);
    subidentifier_len(first_subidentifier(arcs))
        + rest
            .iter()
            .map(|arc| subidentifier_len(u64::from(*arc)))
            .sum::<usize>()
}

fn write_ber_body(arcs: &[u32], out: &mut Vec<u8>) {
    write_subidentifier(first_subidentifier(arcs), out);
    for arc in arcs.get(2..).unwrap_or(&[]) {
        write_subidentifier(u64::from(*arc), out);
    }
}

/// Decode content octets into `arcs` (cleared first). `None` when malformed.
pub(crate) fn decode_ber_body(body: &[u8], arcs: &mut Vec<u32>) -> Option<()> {
    arcs.clear();
    if body.is_empty() {
        return None;
    }
    let mut value: u64 = 0;
    let mut started = false;
    let mut first = true;
    for &octet in body {
        if !started && octet == 0x80 {
            return None;
        }
        started = true;
        value = (value << 7) | u64::from(octet & 0x7F);
        // The first subidentifier holds 40 * 2 + u32::MAX at most.
        if value > u64::from(u32::MAX) + 80 {
            return None;
        }
        if octet & 0x80 != 0 {
            continue;
        }
        if first {
            let (a, b) = match value {
                0..=39 => (0, value),
                40..=79 => (1, value - 40),
                _ => (2, value - 80),
            };
            arcs.push(a as u32);
            arcs.push(u32::try_from(b).ok()?);
            first = false;
        } else {
            arcs.push(u32::try_from(value).ok()?);
        }
        value = 0;
        started = false;
    }
    // The last octet carried the continuation bit.
    if started {
        return None;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn oid(text: &str) -> Oid {
        Oid::parse(text).expect("test OID parses")
    }

    #[rstest]
    #[case::sibling_index_numeric_not_textual("1.3.6.1.2.1.2.2.1.10.9", "1.3.6.1.2.1.2.2.1.10.10")]
    #[case::prefix_before_extension("1.3.6.1.2.1.1", "1.3.6.1.2.1.1.1.0")]
    #[case::column_before_next_column_instance(
        "1.3.6.1.2.1.31.1.1.1.6.10",
        "1.3.6.1.2.1.31.1.1.1.10.1"
    )]
    #[case::system_group_before_interfaces("1.3.6.1.2.1.1.7.0", "1.3.6.1.2.1.2.2.1.8.1")]
    #[case::large_arc_after_small("1.3.6.1.4.1.127", "1.3.6.1.4.1.4294967295")]
    fn ordering_is_by_arc(#[case] lower: &str, #[case] higher: &str) {
        assert!(oid(lower) < oid(higher), "{lower} < {higher}");
    }

    #[rstest]
    #[case::itself("1.3.6.1", "1.3.6.1", true)]
    #[case::shorter("1.3.6", "1.3.6.1", true)]
    #[case::longer("1.3.6.1", "1.3.6", false)]
    #[case::arc_not_text_prefix("1.3.6.1.2.1.31.1.1.1.1", "1.3.6.1.2.1.31.1.1.1.10.1", false)]
    fn prefix_relation(#[case] prefix: &str, #[case] other: &str, #[case] expected: bool) {
        assert_eq!(oid(prefix).is_prefix_of(&oid(other)), expected);
    }

    #[rstest]
    #[case::leading_dot(".1.3.6.1", "starts with '.'")]
    #[case::empty("", "empty arc")]
    #[case::one_arc("1", "one arc")]
    #[case::double_dot("1.3..6", "empty arc")]
    #[case::trailing_dot("1.3.6.", "empty arc")]
    #[case::arc_over_u32("1.3.6.4294967296", "not an unsigned 32-bit integer")]
    #[case::signed_arc("1.3.+6", "not an unsigned 32-bit integer")]
    #[case::negative_arc("1.3.-6", "not an unsigned 32-bit integer")]
    #[case::first_arc_three("3.1", "cannot be encoded")]
    #[case::second_arc_forty_under_one("1.40", "cannot be encoded")]
    fn parse_rejects(#[case] text: &str, #[case] fragment: &str) {
        let err = Oid::parse(text).expect_err("must not parse");
        assert!(err.to_string().contains(fragment), "{err}");
    }

    #[test]
    fn display_round_trips() {
        let text = "1.3.6.1.4.1.8072.3.2.10";
        assert_eq!(oid(text).to_string(), text);
    }

    /// Fixed content octets, each as net-snmp 5.9.4's
    /// `snmpgetnext -v2c -c public 127.0.0.1:1161 <oid>` put it on the wire.
    #[rstest]
    #[case::sys_uptime("1.3.6.1.2.1.1.3.0", &[0x2B, 0x06, 0x01, 0x02, 0x01, 0x01, 0x03, 0x00])]
    #[case::two_octet_arc("1.3.6.1.4.1.8072", &[0x2B, 0x06, 0x01, 0x04, 0x01, 0xBF, 0x08])]
    #[case::three_octet_arc_16384("1.3.6.1.4.1.16384", &[0x2B, 0x06, 0x01, 0x04, 0x01, 0x81, 0x80, 0x00])]
    #[case::five_octet_arc_u32_max("1.3.4294967295", &[0x2B, 0x8F, 0xFF, 0xFF, 0xFF, 0x7F])]
    #[case::first_arc_two_large_second("2.999", &[0x88, 0x37])]
    fn ber_body_fixed_bytes(#[case] text: &str, #[case] expected: &[u8]) {
        let parsed = oid(text);
        let mut out = Vec::new();
        parsed.write_ber_body(&mut out);
        assert_eq!(out, expected, "{text}");
        assert_eq!(parsed.ber_body_len(), expected.len());
    }

    #[rstest]
    #[case::sys_uptime("1.3.6.1.2.1.1.3.0")]
    #[case::three_octet_arc("1.3.6.1.4.1.8072.9999.1.12.16384")]
    #[case::u32_max_arc("1.3.6.1.4.1.8072.9999.1.12.4294967295")]
    #[case::first_arc_two_max("2.4294967215")]
    #[case::zero_zero("0.0")]
    fn ber_body_round_trips(#[case] text: &str) {
        let parsed = oid(text);
        let mut out = Vec::new();
        parsed.write_ber_body(&mut out);
        assert_eq!(Oid::from_ber_body(&out), Some(parsed));
    }

    #[rstest]
    #[case::empty(&[])]
    #[case::unterminated(&[0x2B, 0x86])]
    #[case::leading_0x80_octet(&[0x2B, 0x80, 0x01])]
    #[case::arc_over_u32(&[0x2B, 0x90, 0x80, 0x80, 0x80, 0x00])]
    fn ber_body_rejects(#[case] body: &[u8]) {
        assert_eq!(Oid::from_ber_body(body), None);
    }

    /// `snmpgetnext -v2c -c public 127.0.0.1:1161 .1` (net-snmp 5.9.4) sends
    /// the content octet 0x01, which X.690 reads as 0.1.
    #[test]
    fn net_snmp_single_arc_request_decodes_as_zero_dot_one() {
        assert_eq!(Oid::from_ber_body(&[0x01]), Some(oid("0.1")));
    }

    #[test]
    fn from_arcs_refuses_fewer_than_two() {
        assert_eq!(Oid::from_arcs(vec![1]), None);
        assert_eq!(Oid::from_arcs(Vec::new()), None);
        assert_eq!(Oid::from_arcs(vec![1, 3]), Some(oid("1.3")));
    }

    fn template(text: &str) -> OidTemplate {
        OidTemplate::parse(text).expect("test template parses")
    }

    fn labels(pairs: &[(&str, &str)]) -> Labels {
        Labels::from_pairs(pairs).expect("valid labels")
    }

    #[rustfmt::skip]
    #[rstest]
    #[case::scalar(           "1.3.6.1.4.1.8072.9999.1.1.0",        "1.3.6.1.4.1.8072.9999.1.1.0", &[],               0)]
    #[case::one_index_column( "1.3.6.1.2.1.2.2.1.8.{ifIndex}",      "1.3.6.1.2.1.2.2.1.8",         &["ifIndex"],      1)]
    #[case::two_index_column( "1.3.6.1.4.1.8072.9999.2.1.7.{a}.{b}", "1.3.6.1.4.1.8072.9999.2.1.7", &["a", "b"],       2)]
    #[case::two_arc_object(   "1.3.{x}",                            "1.3",                         &["x"],            1)]
    fn template_splits_object_and_index_arcs(
        #[case] text: &str,
        #[case] object: &str,
        #[case] placeholders: &[&str],
        #[case] index_arcs: u8,
    ) {
        let parsed = template(text);
        assert_eq!(parsed.object(), &oid(object));
        assert_eq!(parsed.placeholders().collect::<Vec<_>>(), placeholders);
        assert_eq!(parsed.index_arcs(), index_arcs);
        assert_eq!(parsed.to_string(), text, "Display gives the template back");
    }

    #[rustfmt::skip]
    #[rstest]
    #[case::string_index(         "1.3.6.1.2.1.31.1.1.1.1.{ifName:str}", "placeholder {ifName:str} is not supported")]
    #[case::any_colon_form(       "1.3.6.1.2.1.31.1.1.1.1.{ifName:x}",   "placeholder {ifName:x} is not supported")]
    #[case::placeholder_mid(      "1.3.6.1.2.1.2.2.1.{col}.1",           "arc \"1\" follows a placeholder")]
    #[case::trailing_dot(         "1.3.6.1.2.1.2.2.1.8.{ifIndex}.",      "arc \"\" follows a placeholder")]
    #[case::starts_with_one(      "{x}.{y}",                             "starts with a placeholder")]
    #[case::empty_placeholder(    "1.3.6.{}",                            "arc \"{}\" is neither a number nor a placeholder")]
    #[case::bad_label_key(        "1.3.6.{9x}",                          "arc \"{9x}\" is neither")]
    #[case::brace_inside_arc(     "1.3.6.1{x}",                          "arc \"1{x}\" is neither")]
    #[case::unclosed(             "1.3.6.{x",                            "arc \"{x\" is neither")]
    #[case::object_one_arc(       "1.{x}",                               "has one arc")]
    #[case::object_not_encodable( "3.40.{x}",                            "cannot be encoded")]
    #[case::object_second_arc_40( "1.40.{x}",                            "cannot be encoded")]
    #[case::object_leading_dot(   ".1.3.6.{x}",                          "starts with '.'")]
    #[case::object_bad_arc(       "1.3.x.{y}",                           "is not an unsigned 32-bit integer")]
    fn template_parse_rejects(#[case] text: &str, #[case] fragment: &str) {
        let err = OidTemplate::parse(text).expect_err("must not parse");
        let shown = err.to_string();
        assert!(shown.contains(fragment), "{shown}");
        assert!(shown.contains(text), "the error names the template: {shown}");
    }

    #[test]
    fn template_object_errors_keep_their_kind() {
        let kind = |text: &str| match OidTemplate::parse(text) {
            Err(OidTemplateError::Object { source, .. }) => source,
            other => panic!("expected an object error, got {other:?}"),
        };
        assert!(matches!(kind("1.{x}"), OidParseError::OneArc(_)));
        assert!(matches!(kind("3.40.{x}"), OidParseError::NotEncodable(_)));
    }

    #[test]
    fn template_arcs_are_capped_at_128() {
        let at_limit = format!("1.3{}", ".{x}".repeat(MAX_ARCS - 2));
        assert_eq!(template(&at_limit).index_arcs() as usize, MAX_ARCS - 2);
        let over = format!("1.3{}", ".{x}".repeat(MAX_ARCS - 1));
        assert!(matches!(
            OidTemplate::parse(&over),
            Err(OidTemplateError::TooManyArcs { arcs: 129, .. })
        ));
    }

    #[test]
    fn render_writes_object_then_label_arcs() {
        let parsed = template("1.3.6.1.4.1.8072.9999.2.1.7.{a}.{b}");
        let mut out = vec![99, 99];
        parsed
            .render(
                &labels(&[("b", "4294967295"), ("a", "3"), ("other", "x")]),
                &mut out,
            )
            .expect("renders");
        assert_eq!(out, [1, 3, 6, 1, 4, 1, 8072, 9999, 2, 1, 7, 3, 4294967295]);
    }

    #[rstest]
    #[case::missing(&[("other", "1")], RenderError::MissingLabel("ifIndex".to_string()))]
    #[case::not_integer(&[("ifIndex", "eth0")], RenderError::NotAnArc { name: "ifIndex".to_string(), value: "eth0".to_string() })]
    #[case::leading_zero(&[("ifIndex", "01")], RenderError::NotAnArc { name: "ifIndex".to_string(), value: "01".to_string() })]
    #[case::over_u32(&[("ifIndex", "4294967296")], RenderError::NotAnArc { name: "ifIndex".to_string(), value: "4294967296".to_string() })]
    fn render_rejects_naming_the_label(
        #[case] pairs: &[(&str, &str)],
        #[case] expected: RenderError,
    ) {
        let mut out = Vec::new();
        let err = template("1.3.6.1.2.1.2.2.1.8.{ifIndex}")
            .render(&labels(pairs), &mut out)
            .expect_err("must not render");
        assert!(err.to_string().contains("ifIndex"), "{err}");
        assert_eq!(err, expected);
    }

    #[rustfmt::skip]
    #[rstest]
    #[case::zero(         "0",          Some(0))]
    #[case::seven(        "7",          Some(7))]
    #[case::u32_max(      "4294967295", Some(u32::MAX))]
    #[case::over_u32(     "4294967296", None)]
    #[case::leading_zero( "01",         None)]
    #[case::double_zero(  "00",         None)]
    #[case::plus_sign(    "+1",         None)]
    #[case::minus_sign(   "-1",         None)]
    #[case::empty(        "",           None)]
    #[case::space(        " 1",         None)]
    #[case::decimal(      "1.0",        None)]
    #[case::exponent(     "1e3",        None)]
    #[case::word(         "eth0",       None)]
    fn index_arc_is_canonical_decimal_u32(#[case] value: &str, #[case] expected: Option<u32>) {
        assert_eq!(index_arc(value), expected);
    }
}
