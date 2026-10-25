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
//! An [`OidTemplate`] is an object's OID followed by its index arcs: one or
//! more placeholders, each one whole trailing arc, or the literal `.0` of a
//! scalar. `1.3.6.1.2.1.31.1.1.1.6.{ifIndex}` renders the event's `ifIndex`
//! label as the last arc; `1.3.6.1.4.1.8072.9999.1.1.0` is the one instance
//! of the scalar `1.3.6.1.4.1.8072.9999.1.1`. A `{label}` value renders as an
//! arc only in the form [`index_arc`] accepts. A `{label:str}` value renders
//! as a string index: its length in bytes, then one arc per byte (RFC 2578
//! §7.7).

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

/// An OID template: an object's OID, then its index arcs.
///
/// `1.3.6.1.2.1.2.2.1.8.{ifIndex}` is the object `1.3.6.1.2.1.2.2.1.8` with
/// one index arc; `1.3.6.1.2.1.31.1.1.1.6.{a}.{b}` has two. A template with
/// no placeholder is a scalar and ends in a literal `.0`, its one index arc:
/// `1.3.6.1.4.1.8072.9999.1.1.0` is the object `1.3.6.1.4.1.8072.9999.1.1`.
/// `1.3.6.1.4.1.8072.9999.3.1.{ifName:str}` has a string index, whose number
/// of arcs depends on the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OidTemplate {
    object: Oid,
    /// Empty for a scalar, whose index arc is the literal `0`.
    placeholders: Vec<Placeholder>,
}

/// One index placeholder: the label it names and how its value renders.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Placeholder {
    name: String,
    /// `{label:str}`: the value's length, then one arc per byte. Otherwise
    /// `{label}`: the value as one [`index_arc`].
    string: bool,
}

impl Placeholder {
    /// The fewest arcs this placeholder renders as: one, or for a string
    /// its length and at least one byte, since an empty value is refused.
    fn min_arcs(&self) -> usize {
        if self.string {
            2
        } else {
            1
        }
    }
}

/// Why an OID template did not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum OidTemplateError {
    /// The object's arcs are not an encodable OID.
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
    /// A placeholder in a form other than `{label}` or `{label:str}`, such
    /// as `{ifName:x}`.
    #[error(
        "OID template {template:?}: placeholder {placeholder} is not supported; an \
         index placeholder is {{label}}, a label whose values are unsigned integers, \
         or {{label:str}}, a label whose values are strings"
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
    /// A template with no placeholder that does not end in `.0`.
    #[error(
        "OID template {template:?} has no placeholder, so it names a scalar, and a \
         scalar's instance ends in .0, as in 1.3.6.1.4.1.8072.9999.1.1.0; a column \
         ends in {{label}} placeholders, as in 1.3.6.1.2.1.2.2.1.8.{{ifIndex}}"
    )]
    ScalarWithoutZero {
        /// The template as written.
        template: String,
    },
    /// More arcs than an OID may have, counting each string index as the
    /// two arcs its shortest value renders.
    #[error("OID template {template:?} has {arcs} arcs; an OID has at most 128")]
    TooManyArcs {
        /// The template as written.
        template: String,
        /// How many arcs it has.
        arcs: usize,
    },
}

/// One arc of an [`OidTemplate`], in order: a literal number or a
/// placeholder's label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TemplateArc<'a> {
    /// A literal arc, such as each arc of the object or a scalar's `0`.
    Literal(u32),
    /// A `{label}` placeholder, by the label it names: one arc.
    Label(&'a str),
    /// A `{label:str}` placeholder, by the label it names: the value's
    /// length, then one arc per byte.
    StringLabel(&'a str),
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
    /// A `{label:str}` placeholder's label is empty; a string index has at
    /// least one byte.
    #[error(
        "label `{0}` is empty, and the OID template's placeholder {{{0}:str}} renders \
         it as a string index, which needs at least one byte"
    )]
    EmptyString(String),
    /// The rendered OID has more arcs than an OID may have.
    #[error(
        "label `{name}` makes the OID {arcs} arcs or more, and an OID has at most 128; \
         its placeholder {{{name}:str}} renders one arc per byte"
    )]
    TooManyArcs {
        /// The string-index label that made the OID too long.
        name: String,
        /// How many arcs the OID had reached.
        arcs: usize,
    },
}

impl OidTemplate {
    /// Parse a template such as `1.3.6.1.2.1.31.1.1.1.6.{ifIndex}`,
    /// `1.3.6.1.4.1.8072.9999.3.1.{ifName:str}` or
    /// `1.3.6.1.4.1.8072.9999.1.1.0`.
    ///
    /// Each placeholder is a whole arc, `{label}` or `{label:str}`, naming a
    /// valid label key, and only placeholders follow the first one. Any other
    /// `{label:...}` form is [`OidTemplateError::Unsupported`]. A template
    /// with no placeholder must end in a literal `.0`
    /// ([`OidTemplateError::ScalarWithoutZero`]). The object, the arcs before
    /// the first placeholder or before a scalar's `.0`, must parse as an
    /// [`Oid`] (at least two arcs, an encodable first pair). The template
    /// has at most [`MAX_ARCS`] arcs, a string index counting as two.
    pub(crate) fn parse(text: &str) -> Result<Self, OidTemplateError> {
        let mut literal: Vec<&str> = Vec::new();
        let mut placeholders: Vec<Placeholder> = Vec::new();
        for arc in text.split('.') {
            let placeholder = match arc.strip_prefix('{').and_then(|a| a.strip_suffix('}')) {
                Some(inner) if inner.contains(':') => match inner.strip_suffix(":str") {
                    Some(name) if is_valid_label_key(name) => Some(Placeholder {
                        name: name.to_owned(),
                        string: true,
                    }),
                    _ => {
                        return Err(OidTemplateError::Unsupported {
                            template: text.to_owned(),
                            placeholder: arc.to_owned(),
                        })
                    }
                },
                Some(inner) if is_valid_label_key(inner) => Some(Placeholder {
                    name: inner.to_owned(),
                    string: false,
                }),
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
                Some(placeholder) => placeholders.push(placeholder),
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
        if placeholders.is_empty() && literal.pop() != Some("0") {
            return Err(OidTemplateError::ScalarWithoutZero {
                template: text.to_owned(),
            });
        }
        let object = Oid::parse(&literal.join(".")).map_err(|source| OidTemplateError::Object {
            template: text.to_owned(),
            source,
        })?;
        let index: usize = placeholders.iter().map(Placeholder::min_arcs).sum();
        let arcs = object.arcs().len() + index.max(1);
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

    /// The labels the placeholders name, in arc order, string indices
    /// included.
    pub(crate) fn placeholders(&self) -> impl Iterator<Item = &str> {
        self.placeholders.iter().map(|p| p.name.as_str())
    }

    /// Whether a placeholder is a string index, `{label:str}`. The number of
    /// index arcs then depends on the label's value, and
    /// [`index_arcs`](Self::index_arcs) is only the fewest.
    pub(crate) fn has_string_index(&self) -> bool {
        self.placeholders.iter().any(|p| p.string)
    }

    /// The object the instances belong to: the arcs before the first
    /// placeholder, or before a scalar's `.0`.
    pub(crate) fn object(&self) -> &Oid {
        &self.object
    }

    /// How many index arcs follow the object: the number of placeholders,
    /// or 1 for a scalar (its `.0`). With a string index
    /// ([`has_string_index`](Self::has_string_index)) it is the fewest a
    /// render produces, each string index counting as two; the arcs a render
    /// produces are its OID's arcs beyond the object's. At most
    /// [`MAX_ARCS`] minus two, so it fits a `u8`.
    pub(crate) fn index_arcs(&self) -> u8 {
        // parse() caps the arcs at MAX_ARCS (128) and the object has two.
        let index: usize = self.placeholders.iter().map(Placeholder::min_arcs).sum();
        index.max(1) as u8
    }

    /// Every arc of the template, in order: the object's arcs, then the
    /// placeholders, or a scalar's literal `0`. A string index is one item
    /// here and several arcs in a rendered OID.
    pub(crate) fn arcs(&self) -> impl Iterator<Item = TemplateArc<'_>> {
        let scalar_zero = self
            .placeholders
            .is_empty()
            .then_some(TemplateArc::Literal(0));
        self.object
            .arcs()
            .iter()
            .map(|arc| TemplateArc::Literal(*arc))
            .chain(scalar_zero)
            .chain(self.placeholders.iter().map(|p| {
                if p.string {
                    TemplateArc::StringLabel(&p.name)
                } else {
                    TemplateArc::Label(&p.name)
                }
            }))
    }

    /// Whether an instance OID this template can render lies under
    /// `prefix`, that is, equals it or starts with it.
    ///
    /// A scalar renders exactly one instance, `object.0`. A column's object
    /// lies under `prefix`, or is a prefix of it with at least as many
    /// placeholders as the arcs it lacks, since a placeholder can render any
    /// arc. A column with a string index, which renders any number of arcs,
    /// counts when its object is a prefix of `prefix` at all. O(arcs).
    pub(crate) fn can_render_under(&self, prefix: &Oid) -> bool {
        let object = self.object.arcs();
        let wanted = prefix.arcs();
        if self.placeholders.is_empty() {
            let mut instance = object.iter().copied().chain(std::iter::once(0));
            return object.len() + 1 >= wanted.len()
                && wanted.iter().all(|arc| instance.next() == Some(*arc));
        }
        if object.len() >= wanted.len() {
            return object.starts_with(wanted);
        }
        wanted.starts_with(object)
            && (self.has_string_index() || object.len() + self.placeholders.len() >= wanted.len())
    }

    /// Write the instance OID's arcs for `labels` into `out` (cleared first):
    /// the object's arcs, then each `{label}` placeholder's value as an arc
    /// and each `{label:str}` placeholder's value as its length in bytes then
    /// one arc per byte, or a scalar's `0`. The index arcs rendered are the
    /// arcs of `out` beyond the object's.
    ///
    /// [`RenderError::MissingLabel`] when `labels` lacks a placeholder's
    /// label, [`RenderError::NotAnArc`] when a `{label}` value is not an
    /// [`index_arc`], [`RenderError::EmptyString`] when a `{label:str}` value
    /// is empty, [`RenderError::TooManyArcs`] when the OID would have more
    /// than [`MAX_ARCS`] arcs.
    pub(crate) fn render(&self, labels: &Labels, out: &mut Vec<u32>) -> Result<(), RenderError> {
        out.clear();
        out.extend_from_slice(self.object.arcs());
        if self.placeholders.is_empty() {
            out.push(0);
        }
        for (i, placeholder) in self.placeholders.iter().enumerate() {
            let name = &placeholder.name;
            let value = labels
                .iter()
                .find_map(|(key, value)| (key == name).then_some(value))
                .ok_or_else(|| RenderError::MissingLabel(name.clone()))?;
            if !placeholder.string {
                let arc = index_arc(value).ok_or_else(|| RenderError::NotAnArc {
                    name: name.clone(),
                    value: value.to_owned(),
                })?;
                out.push(arc);
                continue;
            }
            if value.is_empty() {
                return Err(RenderError::EmptyString(name.clone()));
            }
            // This index, then the fewest the placeholders after it render.
            let after: usize = self.placeholders[i + 1..]
                .iter()
                .map(Placeholder::min_arcs)
                .sum();
            let arcs = out.len() + 1 + value.len() + after;
            if arcs > MAX_ARCS {
                return Err(RenderError::TooManyArcs {
                    name: name.clone(),
                    arcs,
                });
            }
            // At most MAX_ARCS bytes, checked above.
            out.push(value.len() as u32);
            out.extend(value.bytes().map(u32::from));
        }
        Ok(())
    }
}

impl fmt::Display for OidTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.object)?;
        if self.placeholders.is_empty() {
            write!(f, ".0")?;
        }
        for placeholder in &self.placeholders {
            let suffix = if placeholder.string { ":str" } else { "" };
            write!(f, ".{{{}{suffix}}}", placeholder.name)?;
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
    #[case::scalar(           "1.3.6.1.4.1.8072.9999.1.1.0",        "1.3.6.1.4.1.8072.9999.1.1",   &[],               1)]
    #[case::two_arc_scalar(   "1.3.0",                              "1.3",                         &[],               1)]
    #[case::one_index_column( "1.3.6.1.2.1.2.2.1.8.{ifIndex}",      "1.3.6.1.2.1.2.2.1.8",         &["ifIndex"],      1)]
    #[case::two_index_column( "1.3.6.1.4.1.8072.9999.2.1.7.{a}.{b}", "1.3.6.1.4.1.8072.9999.2.1.7", &["a", "b"],       2)]
    #[case::two_arc_object(   "1.3.{x}",                            "1.3",                         &["x"],            1)]
    #[case::string_index(     "1.3.6.1.4.1.8072.9999.3.1.{ifName:str}", "1.3.6.1.4.1.8072.9999.3.1", &["ifName"],      2)]
    #[case::mixed_index(      "1.3.6.1.4.1.8072.9999.3.2.{ifIndex}.{ifName:str}", "1.3.6.1.4.1.8072.9999.3.2", &["ifIndex", "ifName"], 3)]
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
    #[case::any_colon_form(       "1.3.6.1.2.1.31.1.1.1.1.{ifName:x}",   "placeholder {ifName:x} is not supported")]
    #[case::str_bad_label_key(    "1.3.6.1.2.1.31.1.1.1.1.{9x:str}",     "placeholder {9x:str} is not supported")]
    #[case::str_twice(            "1.3.6.1.2.1.31.1.1.1.1.{x:str:str}",  "placeholder {x:str:str} is not supported")]
    #[case::str_mid(              "1.3.6.1.4.1.8072.{ifName:str}.1",     "arc \"1\" follows a placeholder")]
    #[case::placeholder_mid(      "1.3.6.1.2.1.2.2.1.{col}.1",           "arc \"1\" follows a placeholder")]
    #[case::trailing_dot(         "1.3.6.1.2.1.2.2.1.8.{ifIndex}.",      "arc \"\" follows a placeholder")]
    #[case::starts_with_one(      "{x}.{y}",                             "starts with a placeholder")]
    #[case::empty_placeholder(    "1.3.6.{}",                            "arc \"{}\" is neither a number nor a placeholder")]
    #[case::bad_label_key(        "1.3.6.{9x}",                          "arc \"{9x}\" is neither")]
    #[case::brace_inside_arc(     "1.3.6.1{x}",                          "arc \"1{x}\" is neither")]
    #[case::unclosed(             "1.3.6.{x",                            "arc \"{x\" is neither")]
    #[case::object_one_arc(       "1.{x}",                               "has one arc")]
    #[case::scalar_object_one_arc("1.0",                                 "has one arc")]
    #[case::scalar_without_zero(  "1.3.6.1.4.1.8072.9999.1.1",           "has no placeholder, so it names a scalar, and a scalar's instance ends in .0")]
    #[case::scalar_ends_in_one(   "1.3.6.1.2.1.1.3.1",                   "has no placeholder, so it names a scalar")]
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
    fn only_a_string_placeholder_makes_a_string_index() {
        assert!(template("1.3.6.1.4.1.8072.9999.3.1.{ifName:str}").has_string_index());
        assert!(template("1.3.6.1.4.1.8072.9999.3.2.{ifIndex}.{ifName:str}").has_string_index());
        assert!(!template("1.3.6.1.2.1.2.2.1.8.{ifIndex}").has_string_index());
        assert!(!template("1.3.6.1.4.1.8072.9999.1.1.0").has_string_index());
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
    fn a_string_index_counts_two_arcs_toward_the_cap() {
        let at_limit = format!("1.3{}.{{s:str}}", ".{x}".repeat(MAX_ARCS - 4));
        assert_eq!(template(&at_limit).index_arcs() as usize, MAX_ARCS - 2);
        let over = format!("1.3{}.{{s:str}}", ".{x}".repeat(MAX_ARCS - 3));
        assert!(matches!(
            OidTemplate::parse(&over),
            Err(OidTemplateError::TooManyArcs { arcs: 129, .. })
        ));
    }

    /// RFC 2578 §7.7: a variable-length string index is its length, then
    /// one arc per octet. `GigabitEthernet0/0/0` is 20 octets.
    #[test]
    fn a_string_index_renders_its_length_then_one_arc_per_byte() {
        let value = "GigabitEthernet0/0/0";
        assert_eq!(value.len(), 20, "fixture present");
        let parsed = template("1.3.6.1.4.1.8072.9999.3.1.{ifName:str}");
        let mut out = vec![99];
        parsed
            .render(&labels(&[("ifName", value)]), &mut out)
            .expect("renders");
        assert_eq!(
            out,
            [
                1, 3, 6, 1, 4, 1, 8072, 9999, 3, 1, // the object
                20, 71, 105, 103, 97, 98, 105, 116, 69, 116, 104, 101, 114, 110, 101, 116, 48, 47,
                48, 47, 48,
            ]
        );
        assert_eq!(
            out.len() - parsed.object().arcs().len(),
            21,
            "21 index arcs"
        );
    }

    #[test]
    fn a_template_mixing_an_integer_and_a_string_index_renders_both() {
        let parsed = template("1.3.6.1.4.1.8072.9999.3.2.{ifIndex}.{ifName:str}");
        let mut out = Vec::new();
        parsed
            .render(&labels(&[("ifName", "Gi0"), ("ifIndex", "7")]), &mut out)
            .expect("renders");
        assert_eq!(out, [1, 3, 6, 1, 4, 1, 8072, 9999, 3, 2, 7, 3, 71, 105, 48]);
        assert_eq!(
            parsed.to_string(),
            "1.3.6.1.4.1.8072.9999.3.2.{ifIndex}.{ifName:str}"
        );
    }

    #[test]
    fn a_string_index_takes_any_text_an_integer_index_refuses() {
        let parsed = template("1.3.6.1.4.1.8072.9999.3.1.{v:str}");
        let mut out = Vec::new();
        for value in ["01", "-1", "eth 0", "é"] {
            parsed
                .render(&labels(&[("v", value)]), &mut out)
                .unwrap_or_else(|e| panic!("{value:?} renders: {e}"));
            assert_eq!(
                out[10] as usize,
                value.len(),
                "{value:?}: its length in bytes"
            );
            assert_eq!(out.len(), 11 + value.len(), "{value:?}: one arc per byte");
        }
    }

    #[test]
    fn an_empty_string_index_is_refused_naming_the_label() {
        let mut out = Vec::new();
        let parsed = template("1.3.6.1.4.1.8072.9999.3.1.{ifName:str}");
        parsed
            .render(&labels(&[("ifName", "x")]), &mut out)
            .expect("one byte renders");
        let err = parsed
            .render(&labels(&[("ifName", "")]), &mut out)
            .expect_err("an empty string index");
        assert_eq!(err, RenderError::EmptyString("ifName".to_string()));
        assert!(err.to_string().contains("`ifName` is empty"), "{err}");
    }

    #[test]
    fn a_string_index_that_overflows_the_oid_is_refused() {
        let parsed = template("1.3.6.1.4.1.8072.9999.3.1.{ifName:str}");
        let mut out = Vec::new();
        // The object has 10 arcs and the length one: 117 bytes fill 128.
        let fits = "x".repeat(MAX_ARCS - 11);
        parsed
            .render(&labels(&[("ifName", &fits)]), &mut out)
            .expect("128 arcs render");
        assert_eq!(out.len(), MAX_ARCS);
        let over = "x".repeat(MAX_ARCS - 10);
        let err = parsed
            .render(&labels(&[("ifName", &over)]), &mut out)
            .expect_err("129 arcs");
        assert_eq!(
            err,
            RenderError::TooManyArcs {
                name: "ifName".to_string(),
                arcs: 129
            }
        );
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

    #[test]
    fn arcs_list_the_object_then_the_index() {
        use TemplateArc::{Label, Literal, StringLabel};
        let column = template("1.3.6.{a}.{b}");
        let scalar = template("1.3.6.0");
        let string = template("1.3.6.{a}.{b:str}");
        assert_eq!(
            column.arcs().collect::<Vec<_>>(),
            [Literal(1), Literal(3), Literal(6), Label("a"), Label("b")]
        );
        assert_eq!(
            string.arcs().collect::<Vec<_>>(),
            [
                Literal(1),
                Literal(3),
                Literal(6),
                Label("a"),
                StringLabel("b")
            ]
        );
        assert_eq!(
            scalar.arcs().collect::<Vec<_>>(),
            [Literal(1), Literal(3), Literal(6), Literal(0)]
        );
    }

    #[test]
    fn a_scalar_renders_its_object_then_zero() {
        let parsed = template("1.3.6.1.4.1.8072.9999.1.1.0");
        let mut out = vec![99];
        parsed
            .render(&labels(&[("device", "rtr-1")]), &mut out)
            .expect("a scalar needs no label");
        assert_eq!(out, [1, 3, 6, 1, 4, 1, 8072, 9999, 1, 1, 0]);
    }

    #[rustfmt::skip]
    #[rstest]
    #[case::scalar_under(             "1.3.6.1.2.1.1.5.0",             "1.3.6.1.2.1.1",     true)]
    #[case::scalar_beside_it(         "1.3.6.1.2.1.0",                 "1.3.6.1.2.1.1",     false)]
    #[case::scalar_elsewhere(         "1.3.6.1.4.1.8072.9999.1.1.0",   "1.3.6.1.2.1.1",     false)]
    #[case::scalar_is_the_prefix(     "1.3.6.1.2.1.1.3.0",             "1.3.6.1.2.1.1.3.0", true)]
    #[case::scalar_object_is_not(     "1.3.6.1.2.1.1.3.0",             "1.3.6.1.2.1.1.3.1", false)]
    #[case::column_under(             "1.3.6.1.2.1.1.9.1.2.{x}",       "1.3.6.1.2.1.1",     true)]
    #[case::column_reaches_it(        "1.3.6.1.2.1.{x}",               "1.3.6.1.2.1.1",     true)]
    #[case::column_two_short(         "1.3.6.1.2.{x}",                 "1.3.6.1.2.1.1",     false)]
    #[case::column_reaches_with_two(  "1.3.6.1.2.{x}.{y}",             "1.3.6.1.2.1.1",     true)]
    #[case::column_beside(            "1.3.6.1.2.1.2.2.1.8.{ifIndex}", "1.3.6.1.2.1.1",     false)]
    #[case::string_reaches_any_depth( "1.3.6.1.2.{x:str}",             "1.3.6.1.2.1.1",     true)]
    #[case::string_beside(            "1.3.6.1.2.1.31.1.1.1.1.{x:str}", "1.3.6.1.2.1.1",    false)]
    fn can_render_under_a_prefix(#[case] text: &str, #[case] prefix: &str, #[case] expected: bool) {
        assert_eq!(template(text).can_render_under(&oid(prefix)), expected, "{text} under {prefix}");
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
