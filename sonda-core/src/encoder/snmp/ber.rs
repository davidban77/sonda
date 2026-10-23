//! The subset of ASN.1 BER that SNMPv1 and SNMPv2c messages use.
//!
//! Writers append to a caller's `Vec<u8>`; readers borrow from the datagram.
//! Lengths are definite: short form below 128, otherwise long form with the
//! fewest length octets. The reader also accepts a long form with leading
//! zero octets, which BER permits, and refuses the indefinite form, which
//! SNMP does not use.
//!
//! # Message layout
//!
//! ```text
//! Message:  SEQUENCE { version INTEGER, community OCTET STRING, PDU }
//! PDU:      [tag] { request-id INTEGER, error-status INTEGER, error-index INTEGER,
//!                   SEQUENCE OF VarBind }
//! GetBulk:  [0xA5] { request-id, non-repeaters, max-repetitions, SEQUENCE OF VarBind }
//! VarBind:  SEQUENCE { OBJECT IDENTIFIER, value | NULL | exception }
//! ```

use super::oid::{self, Oid};

/// A BER tag in the SNMP subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tag {
    /// INTEGER, 0x02.
    Integer,
    /// OCTET STRING, 0x04.
    OctetString,
    /// NULL, 0x05.
    Null,
    /// OBJECT IDENTIFIER, 0x06.
    Oid,
    /// SEQUENCE, 0x30.
    Sequence,
    /// IpAddress, 0x40.
    IpAddress,
    /// Counter32, 0x41.
    Counter32,
    /// Gauge32 and Unsigned32, 0x42.
    Gauge32,
    /// TimeTicks, 0x43.
    TimeTicks,
    /// Opaque, 0x44.
    Opaque,
    /// Counter64, 0x46.
    Counter64,
    /// noSuchObject exception, 0x80.
    NoSuchObject,
    /// noSuchInstance exception, 0x81.
    NoSuchInstance,
    /// endOfMibView exception, 0x82.
    EndOfMibView,
    /// GetRequest-PDU, 0xA0.
    GetRequest,
    /// GetNextRequest-PDU, 0xA1.
    GetNextRequest,
    /// Response-PDU, 0xA2.
    Response,
    /// SetRequest-PDU, 0xA3.
    SetRequest,
    /// GetBulkRequest-PDU, 0xA5.
    GetBulkRequest,
    /// InformRequest-PDU, 0xA6.
    InformRequest,
    /// SNMPv2-Trap-PDU, 0xA7.
    TrapV2,
    /// Report-PDU, 0xA8.
    Report,
}

impl Tag {
    /// The identifier octet.
    pub(crate) const fn byte(self) -> u8 {
        match self {
            Tag::Integer => 0x02,
            Tag::OctetString => 0x04,
            Tag::Null => 0x05,
            Tag::Oid => 0x06,
            Tag::Sequence => 0x30,
            Tag::IpAddress => 0x40,
            Tag::Counter32 => 0x41,
            Tag::Gauge32 => 0x42,
            Tag::TimeTicks => 0x43,
            Tag::Opaque => 0x44,
            Tag::Counter64 => 0x46,
            Tag::NoSuchObject => 0x80,
            Tag::NoSuchInstance => 0x81,
            Tag::EndOfMibView => 0x82,
            Tag::GetRequest => 0xA0,
            Tag::GetNextRequest => 0xA1,
            Tag::Response => 0xA2,
            Tag::SetRequest => 0xA3,
            Tag::GetBulkRequest => 0xA5,
            Tag::InformRequest => 0xA6,
            Tag::TrapV2 => 0xA7,
            Tag::Report => 0xA8,
        }
    }

    /// The tag an identifier octet names, or [`BerError::BadTag`].
    pub(crate) const fn from_byte(byte: u8) -> Result<Tag, BerError> {
        Ok(match byte {
            0x02 => Tag::Integer,
            0x04 => Tag::OctetString,
            0x05 => Tag::Null,
            0x06 => Tag::Oid,
            0x30 => Tag::Sequence,
            0x40 => Tag::IpAddress,
            0x41 => Tag::Counter32,
            0x42 => Tag::Gauge32,
            0x43 => Tag::TimeTicks,
            0x44 => Tag::Opaque,
            0x46 => Tag::Counter64,
            0x80 => Tag::NoSuchObject,
            0x81 => Tag::NoSuchInstance,
            0x82 => Tag::EndOfMibView,
            0xA0 => Tag::GetRequest,
            0xA1 => Tag::GetNextRequest,
            0xA2 => Tag::Response,
            0xA3 => Tag::SetRequest,
            0xA5 => Tag::GetBulkRequest,
            0xA6 => Tag::InformRequest,
            0xA7 => Tag::TrapV2,
            0xA8 => Tag::Report,
            other => return Err(BerError::BadTag(other)),
        })
    }

    /// Whether this tag is one of the PDU tags.
    pub(crate) const fn is_pdu(self) -> bool {
        matches!(
            self,
            Tag::GetRequest
                | Tag::GetNextRequest
                | Tag::Response
                | Tag::SetRequest
                | Tag::GetBulkRequest
                | Tag::InformRequest
                | Tag::TrapV2
                | Tag::Report
        )
    }
}

/// Why BER input did not decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum BerError {
    /// The input ends inside a tag, a length or a body.
    #[error("truncated BER input")]
    Truncated,
    /// An indefinite or oversized length, a primitive of a length its type
    /// cannot have, or bytes left over where none may be.
    #[error("bad BER length")]
    BadLength,
    /// A tag outside the subset, or not the one the position requires.
    #[error("unexpected BER tag 0x{0:02x}")]
    BadTag(u8),
    /// An integer that does not fit the type it is read as.
    #[error("BER integer out of range")]
    Overflow,
    /// OBJECT IDENTIFIER content octets that are not a valid OID.
    #[error("malformed BER object identifier")]
    BadOid,
}

/// Octets a definite length takes, the identifier octet excluded.
pub(crate) const fn length_len(len: usize) -> usize {
    if len < 0x80 {
        1
    } else {
        1 + (usize::BITS - len.leading_zeros()).div_ceil(8) as usize
    }
}

/// Octets a whole TLV with a body of `body_len` octets takes.
pub(crate) const fn tlv_len(body_len: usize) -> usize {
    1 + length_len(body_len) + body_len
}

/// Write a definite length.
fn write_length(out: &mut Vec<u8>, len: usize) {
    if len < 0x80 {
        out.push(len as u8);
        return;
    }
    let octets = length_len(len) - 1;
    out.push(0x80 | octets as u8);
    let be = len.to_be_bytes();
    out.extend_from_slice(&be[be.len() - octets..]);
}

/// Write a tag and a definite length.
pub(crate) fn write_header(out: &mut Vec<u8>, tag: Tag, len: usize) {
    out.push(tag.byte());
    write_length(out, len);
}

/// Write a constructed TLV whose body `body` appends; the length is
/// back-patched, moving the body when it needs the long form.
pub(crate) fn write_tlv(out: &mut Vec<u8>, tag: Tag, body: impl FnOnce(&mut Vec<u8>)) {
    out.push(tag.byte());
    let at = out.len();
    out.push(0);
    body(out);
    let len = out.len() - at - 1;
    if len < 0x80 {
        out[at] = len as u8;
        return;
    }
    let octets = length_len(len) - 1;
    let end = out.len();
    out.resize(end + octets, 0);
    out.copy_within(at + 1..end, at + 1 + octets);
    out[at] = 0x80 | octets as u8;
    let be = len.to_be_bytes();
    out[at + 1..at + 1 + octets].copy_from_slice(&be[be.len() - octets..]);
}

/// The minimal two's-complement content octets of `v`, as a range of its
/// big-endian bytes.
fn signed_span(v: i64) -> ([u8; 8], usize) {
    let bytes = v.to_be_bytes();
    let mut start = 0;
    while start < 7 {
        let redundant = (bytes[start] == 0x00 && bytes[start + 1] & 0x80 == 0)
            || (bytes[start] == 0xFF && bytes[start + 1] & 0x80 != 0);
        if !redundant {
            break;
        }
        start += 1;
    }
    (bytes, start)
}

/// Octets a signed integer TLV takes.
pub(crate) fn i64_len(v: i64) -> usize {
    let (_, start) = signed_span(v);
    tlv_len(8 - start)
}

/// Write `v` as a minimal two's-complement integer with `tag`.
pub(crate) fn write_i64(out: &mut Vec<u8>, tag: Tag, v: i64) {
    let (bytes, start) = signed_span(v);
    write_header(out, tag, 8 - start);
    out.extend_from_slice(&bytes[start..]);
}

/// Write `v` as an unsigned integer with `tag`: minimal octets, with a
/// leading zero octet when the first would have its high bit set, so
/// 2^63 is `00 80 00 00 00 00 00 00 00`.
pub(crate) fn write_u64(out: &mut Vec<u8>, tag: Tag, v: u64) {
    let bytes = v.to_be_bytes();
    let start = (v.leading_zeros() / 8).min(7) as usize;
    let pad = bytes[start] & 0x80 != 0;
    write_header(out, tag, 8 - start + usize::from(pad));
    if pad {
        out.push(0);
    }
    out.extend_from_slice(&bytes[start..]);
}

/// Write `bytes` as a primitive with `tag`.
pub(crate) fn write_octets(out: &mut Vec<u8>, tag: Tag, bytes: &[u8]) {
    write_header(out, tag, bytes.len());
    out.extend_from_slice(bytes);
}

/// Write NULL: `05 00`.
pub(crate) fn write_null(out: &mut Vec<u8>) {
    out.extend_from_slice(&[Tag::Null.byte(), 0]);
}

/// Write an OBJECT IDENTIFIER.
pub(crate) fn write_oid(out: &mut Vec<u8>, oid: &Oid) {
    write_header(out, Tag::Oid, oid.ber_body_len());
    oid.write_ber_body(out);
}

/// Octets a VarBind of `oid` and an already-encoded value TLV takes.
pub(crate) fn varbind_len(oid: &Oid, value_len: usize) -> usize {
    tlv_len(tlv_len(oid.ber_body_len()) + value_len)
}

/// Write a VarBind of `oid` and an already-encoded value TLV.
pub(crate) fn write_varbind(out: &mut Vec<u8>, oid: &Oid, value: &[u8]) {
    let body = tlv_len(oid.ber_body_len()) + value.len();
    write_header(out, Tag::Sequence, body);
    write_oid(out, oid);
    out.extend_from_slice(value);
}

/// A cursor over consecutive TLVs, borrowing their bodies.
pub(crate) struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    /// A reader over `input`.
    pub(crate) fn new(input: &'a [u8]) -> Self {
        Reader { rest: input }
    }

    /// Whether every TLV has been read.
    pub(crate) fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    /// The next TLV's identifier octet and body, whatever the tag.
    pub(crate) fn raw_tlv(&mut self) -> Result<(u8, &'a [u8]), BerError> {
        let (&tag, rest) = self.rest.split_first().ok_or(BerError::Truncated)?;
        if tag & 0x1F == 0x1F {
            // High-tag-number form; nothing in SNMP uses it.
            return Err(BerError::BadTag(tag));
        }
        let (&first, mut rest) = rest.split_first().ok_or(BerError::Truncated)?;
        let len = if first < 0x80 {
            usize::from(first)
        } else {
            let octets = usize::from(first & 0x7F);
            // 0x80 is the indefinite form; more than four octets is no datagram.
            if octets == 0 || octets > 4 {
                return Err(BerError::BadLength);
            }
            if rest.len() < octets {
                return Err(BerError::Truncated);
            }
            let (len_bytes, after) = rest.split_at(octets);
            rest = after;
            len_bytes
                .iter()
                .fold(0usize, |acc, b| (acc << 8) | usize::from(*b))
        };
        if rest.len() < len {
            return Err(BerError::Truncated);
        }
        let (body, after) = rest.split_at(len);
        self.rest = after;
        Ok((tag, body))
    }

    /// The next TLV's tag and body.
    pub(crate) fn tlv(&mut self) -> Result<(Tag, &'a [u8]), BerError> {
        let (tag, body) = self.raw_tlv()?;
        Ok((Tag::from_byte(tag)?, body))
    }

    /// The next TLV's body, which must carry `tag`.
    pub(crate) fn expect(&mut self, tag: Tag) -> Result<&'a [u8], BerError> {
        let (found, body) = self.raw_tlv()?;
        if found != tag.byte() {
            return Err(BerError::BadTag(found));
        }
        Ok(body)
    }

    /// The next TLV, which must carry `tag`, as a signed integer.
    pub(crate) fn i64(&mut self, tag: Tag) -> Result<i64, BerError> {
        decode_i64(self.expect(tag)?)
    }

    /// The next TLV, which must carry `tag`, as an unsigned integer.
    pub(crate) fn u64(&mut self, tag: Tag) -> Result<u64, BerError> {
        decode_u64(self.expect(tag)?)
    }

    /// The next TLV, which must carry `tag`, as raw octets.
    pub(crate) fn octets(&mut self, tag: Tag) -> Result<&'a [u8], BerError> {
        self.expect(tag)
    }

    /// The next TLV as an OBJECT IDENTIFIER.
    pub(crate) fn oid(&mut self) -> Result<Oid, BerError> {
        Oid::from_ber_body(self.expect(Tag::Oid)?).ok_or(BerError::BadOid)
    }

    /// The next TLV as NULL.
    pub(crate) fn null(&mut self) -> Result<(), BerError> {
        if !self.expect(Tag::Null)?.is_empty() {
            return Err(BerError::BadLength);
        }
        Ok(())
    }
}

/// Decode two's-complement content octets.
pub(crate) fn decode_i64(body: &[u8]) -> Result<i64, BerError> {
    match body.len() {
        0 => Err(BerError::BadLength),
        1..=8 => {
            let negative = body[0] & 0x80 != 0;
            let mut bytes = if negative { [0xFF; 8] } else { [0; 8] };
            bytes[8 - body.len()..].copy_from_slice(body);
            Ok(i64::from_be_bytes(bytes))
        }
        _ => Err(BerError::Overflow),
    }
}

/// Decode unsigned content octets: at most eight significant octets, after
/// at most one leading zero octet.
pub(crate) fn decode_u64(body: &[u8]) -> Result<u64, BerError> {
    let significant = match body {
        [] => return Err(BerError::BadLength),
        [0, rest @ ..] if !rest.is_empty() => rest,
        _ => body,
    };
    if significant.len() > 8 {
        return Err(BerError::Overflow);
    }
    Ok(significant
        .iter()
        .fold(0u64, |acc, b| (acc << 8) | u64::from(*b)))
}

/// A decoded SNMPv1 or SNMPv2c message, borrowing from the datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Message<'a> {
    /// 0 for SNMPv1, 1 for SNMPv2c.
    pub(crate) version: i64,
    /// The community string.
    pub(crate) community: &'a [u8],
    /// The PDU tag.
    pub(crate) pdu: Tag,
    /// The request-id, echoed in the response.
    pub(crate) request_id: i64,
    /// error-status; non-repeaters in a GetBulkRequest.
    pub(crate) error_status: i64,
    /// error-index; max-repetitions in a GetBulkRequest.
    pub(crate) error_index: i64,
    /// The body of the varbind list SEQUENCE.
    pub(crate) varbinds: &'a [u8],
}

impl<'a> Message<'a> {
    /// Decode a v1/v2c message. Bytes after the outer SEQUENCE, or after the
    /// varbind list inside the PDU, are [`BerError::BadLength`]. A v3
    /// message fails with `BadTag(0x30)` where the community would be.
    pub(crate) fn decode(datagram: &'a [u8]) -> Result<Self, BerError> {
        let mut outer = Reader::new(datagram);
        let body = outer.expect(Tag::Sequence)?;
        if !outer.is_empty() {
            return Err(BerError::BadLength);
        }
        let mut message = Reader::new(body);
        let version = message.i64(Tag::Integer)?;
        let community = message.octets(Tag::OctetString)?;
        let (pdu, pdu_body) = message.tlv()?;
        if !pdu.is_pdu() {
            return Err(BerError::BadTag(pdu.byte()));
        }
        if !message.is_empty() {
            return Err(BerError::BadLength);
        }
        let mut fields = Reader::new(pdu_body);
        let request_id = fields.i64(Tag::Integer)?;
        let error_status = fields.i64(Tag::Integer)?;
        let error_index = fields.i64(Tag::Integer)?;
        let varbinds = fields.expect(Tag::Sequence)?;
        if !fields.is_empty() {
            return Err(BerError::BadLength);
        }
        Ok(Message {
            version,
            community,
            pdu,
            request_id,
            error_status,
            error_index,
            varbinds,
        })
    }

    /// The varbinds, in order.
    pub(crate) fn varbinds(&self) -> VarBinds<'a> {
        VarBinds {
            reader: Reader::new(self.varbinds),
        }
    }
}

/// One VarBind, borrowing from the datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VarBind<'a> {
    /// The OBJECT IDENTIFIER's content octets.
    pub(crate) oid: &'a [u8],
    /// The value's identifier octet.
    pub(crate) value_tag: u8,
    /// The value's content octets.
    pub(crate) value: &'a [u8],
}

impl VarBind<'_> {
    /// The name, decoded.
    pub(crate) fn name(&self) -> Result<Oid, BerError> {
        Oid::from_ber_body(self.oid).ok_or(BerError::BadOid)
    }

    /// The name's arcs, decoded into `arcs` (cleared first).
    pub(crate) fn name_into(&self, arcs: &mut Vec<u32>) -> Result<(), BerError> {
        oid::decode_ber_body(self.oid, arcs).ok_or(BerError::BadOid)
    }
}

/// An iterator over a varbind list's VarBinds.
pub(crate) struct VarBinds<'a> {
    reader: Reader<'a>,
}

impl<'a> Iterator for VarBinds<'a> {
    type Item = Result<VarBind<'a>, BerError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.reader.is_empty() {
            return None;
        }
        Some(self.read_one())
    }
}

impl<'a> VarBinds<'a> {
    fn read_one(&mut self) -> Result<VarBind<'a>, BerError> {
        let body = self.reader.expect(Tag::Sequence)?;
        let mut fields = Reader::new(body);
        let oid = fields.expect(Tag::Oid)?;
        let (value_tag, value) = fields.raw_tlv()?;
        if !fields.is_empty() {
            return Err(BerError::BadLength);
        }
        Ok(VarBind {
            oid,
            value_tag,
            value,
        })
    }
}

/// Octets a v1/v2c message takes whose varbind list body is
/// `varbinds_len` octets.
pub(crate) fn message_len(
    version: i64,
    community: &[u8],
    request_id: i64,
    error_status: i64,
    error_index: i64,
    varbinds_len: usize,
) -> usize {
    let pdu_body =
        i64_len(request_id) + i64_len(error_status) + i64_len(error_index) + tlv_len(varbinds_len);
    tlv_len(i64_len(version) + tlv_len(community.len()) + tlv_len(pdu_body))
}

/// Write a v1/v2c message whose varbind list body `varbinds` appends.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_message(
    out: &mut Vec<u8>,
    version: i64,
    community: &[u8],
    pdu: Tag,
    request_id: i64,
    error_status: i64,
    error_index: i64,
    varbinds: impl FnOnce(&mut Vec<u8>),
) {
    write_tlv(out, Tag::Sequence, |out| {
        write_i64(out, Tag::Integer, version);
        write_octets(out, Tag::OctetString, community);
        write_tlv(out, pdu, |out| {
            write_i64(out, Tag::Integer, request_id);
            write_i64(out, Tag::Integer, error_status);
            write_i64(out, Tag::Integer, error_index);
            write_tlv(out, Tag::Sequence, varbinds);
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn hex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text.bytes().filter(|b| b.is_ascii_hexdigit()).collect();
        digits
            .chunks(2)
            .map(|pair| {
                let s = std::str::from_utf8(pair).expect("hex digits are ASCII");
                u8::from_str_radix(s, 16).expect("hex digits")
            })
            .collect()
    }

    /// `snmpget -v2c -c public 127.0.0.1:1161 1.3.6.1.2.1.1.3.0`, net-snmp 5.9.4.
    const NET_SNMP_GET: &str =
        "302902010104067075626c6963a01c02043ac25c77020100020100300e300c06082b060102010103\
        000500";

    /// This codec's reply, which snmpget printed as `Timeticks: (852)`.
    const NET_SNMP_GET_REPLY: &str =
        "302b02010104067075626c6963a21e02043ac25c770201000201003010300e06082b060102010103\
        0043020354";

    /// `snmpgetnext -v2c -c public 127.0.0.1:1161 1.3.6.1.2.1.31.1.1.1.6`, net-snmp 5.9.4.
    const NET_SNMP_GETNEXT: &str =
        "302b02010104067075626c6963a11e02042db414d60201000201003010300e060a2b060102011f01\
        0101060500";

    /// This codec's reply, which snmpgetnext printed as
    /// `…6.1 = Counter64: 1001000`.
    const NET_SNMP_GETNEXT_REPLY: &str =
        "302f02010104067075626c6963a22202042db414d602010002010030143012060b2b060102011f01\
        0101060146030f4628";

    /// `snmpbulkget -v2c -c public -Cr10 127.0.0.1:1161 1.3.6.1.2.1.31.1.1.1.6`, net-snmp 5.9.4.
    const NET_SNMP_GETBULK: &str =
        "302b02010104067075626c6963a51e02041573e9b502010002010a3010300e060a2b060102011f01\
        0101060500";

    /// This codec's reply, which snmpbulkget printed as ten Counter64
    /// instances, the last 9223372036855275808 (2^63 + 500000).
    const NET_SNMP_GETBULK_REPLY: &str =
        "3081ec02010104067075626c6963a281de02041573e9b50201000201003081cf3012060b2b060102\
        011f010101060146030f46283012060b2b060102011f010101060246031e8c503012060b2b060102\
        011f010101060346032dd2783012060b2b060102011f010101060446033d18a03012060b2b060102\
        011f010101060546034c5ec83012060b2b060102011f010101060646035ba4f03012060b2b060102\
        011f010101060746036aeb183012060b2b060102011f010101060846037a31403013060b2b060102\
        011f01010106094604008977683018060b2b060102011f010101060a460900800000000007a120";

    /// Telegraf 1.40.2 `inputs.snmp` field `.1.3.6.1.2.1.1.3.0` (gosnmp), version 2, community public.
    const GOSNMP_GET: &str =
        "302902010104067075626c6963a01c02044033a1b4020100020100300e300c06082b060102010103\
        000500";

    /// Telegraf 1.40.2 `inputs.snmp` table field `.1.3.6.1.2.1.31.1.1.1.6` (gosnmp BulkWalk, default max_repetitions).
    const GOSNMP_GETBULK: &str =
        "302b02010104067075626c6963a51e02044033a1bc02010002010a3010300e060a2b060102011f01\
        0101060500";

    /// This codec's tooBig reply to a gosnmp GET under a 200-byte budget.
    const TOO_BIG_REPLY: &str = "301b02010104067075626c6963a20e02042c261a7f0201010201003000";

    /// `snmptrap -v2c -c public 127.0.0.1:1161 '' 1.3.6.1.4.1.8072.9999.0.1` with the playpen values as varbinds (`i -1`, `i -128`, `i -129`, `i -2147483648`, `i 2147483647`, `C 9223372036854775808`, `C 18446744073709551615`, `c 4294967295`, `u 0`, `t 4294967295`, `s` of 128 `b`s, `o 1.3.6.1.4.1.4294967295.16384`), net-snmp 5.9.4: every value as net-snmp encodes it.
    const NET_SNMP_TRAP: &str =
        "308201db02010104067075626c6963a78201cc0204385d9c89020100020100308201bc300f06082b\
        06010201010300430301c1a73019060a2b060106030101040100060b2b06010401bf08ce0f000130\
        11060c2b06010401bf08ce0f0101000201ff3011060c2b06010401bf08ce0f010200020180301206\
        0c2b06010401bf08ce0f0103000202ff7f3014060c2b06010401bf08ce0f01040002048000000030\
        14060c2b06010401bf08ce0f01050002047fffffff3019060c2b06010401bf08ce0f010600460900\
        80000000000000003019060c2b06010401bf08ce0f010700460900ffffffffffffffff3015060c2b\
        06010401bf08ce0f010800410500ffffffff3011060c2b06010401bf08ce0f010900420100301506\
        0c2b06010401bf08ce0f010a00430500ffffffff308191060c2b06010401bf08ce0f010c00048180\
        62626262626262626262626262626262626262626262626262626262626262626262626262626262\
        62626262626262626262626262626262626262626262626262626262626262626262626262626262\
        62626262626262626262626262626262626262626262626262626262626262626262626262626262\
        6262626262626262301d060c2b06010401bf08ce0f010e00060d2b060104018fffffff7f818000";

    #[rustfmt::skip]
    #[rstest]
    #[case::zero(            0,                     "020100")]
    #[case::max_one_octet(   127,                   "02017f")]
    #[case::needs_sign_pad(  128,                   "02020080")]
    #[case::minus_one(       -1,                    "0201ff")]
    #[case::minus_128(       -128,                  "020180")]
    #[case::minus_129(       -129,                  "0202ff7f")]
    #[case::i32_min(         i32::MIN as i64,       "020480000000")]
    #[case::i32_max(         i32::MAX as i64,       "02047fffffff")]
    #[case::i64_min(         i64::MIN,              "02088000000000000000")]
    #[case::i64_max(         i64::MAX,              "02087fffffffffffffff")]
    fn integer_minimal_twos_complement(#[case] value: i64, #[case] expected: &str) {
        let mut out = Vec::new();
        write_i64(&mut out, Tag::Integer, value);
        assert_eq!(out, hex(expected));
        assert_eq!(out.len(), i64_len(value));
        assert_eq!(Reader::new(&out).i64(Tag::Integer), Ok(value));
    }

    #[rustfmt::skip]
    #[rstest]
    #[case::zero(            Tag::Gauge32,   0,              "420100")]
    #[case::high_bit_pad(    Tag::Counter32, 0x80,           "41020080")]
    #[case::u32_max(         Tag::Counter32, u32::MAX as u64,"410500ffffffff")]
    #[case::two_pow_63(      Tag::Counter64, 1 << 63,        "4609008000000000000000")]
    #[case::u64_max(         Tag::Counter64, u64::MAX,       "460900ffffffffffffffff")]
    #[case::timeticks(       Tag::TimeTicks, 12_345,         "43023039")]
    fn unsigned_leading_zero_when_high_bit_set(#[case] tag: Tag, #[case] value: u64, #[case] expected: &str) {
        let mut out = Vec::new();
        write_u64(&mut out, tag, value);
        assert_eq!(out, hex(expected));
        assert_eq!(Reader::new(&out).u64(tag), Ok(value));
    }

    #[rstest]
    #[case::short_max(127, "7f")]
    #[case::long_one_octet(128, "8180")]
    #[case::long_one_octet_max(255, "81ff")]
    #[case::long_two_octets(256, "820100")]
    #[case::long_three_octets(65_536, "83010000")]
    fn lengths_short_and_long_form(#[case] len: usize, #[case] expected_length: &str) {
        let body = vec![0xAB; len];
        let mut written = Vec::new();
        write_octets(&mut written, Tag::OctetString, &body);
        let mut patched = Vec::new();
        write_tlv(&mut patched, Tag::OctetString, |out| {
            out.extend_from_slice(&body)
        });
        let mut expected = vec![0x04];
        expected.extend(hex(expected_length));
        expected.extend_from_slice(&body);
        assert_eq!(written, expected, "write_octets");
        assert_eq!(patched, expected, "write_tlv back-patch");
        assert_eq!(tlv_len(len), expected.len());
        let mut reader = Reader::new(&written);
        assert_eq!(reader.octets(Tag::OctetString), Ok(&body[..]));
        assert!(reader.is_empty());
    }

    #[test]
    fn reader_accepts_non_minimal_long_form() {
        // A 3-byte body announced with two length octets.
        let input = hex("30820003 020101");
        let mut reader = Reader::new(&input);
        assert_eq!(reader.expect(Tag::Sequence), Ok(&hex("020101")[..]));
    }

    #[rstest]
    #[case::empty("", BerError::Truncated)]
    #[case::no_length("02", BerError::Truncated)]
    #[case::body_short("020201", BerError::Truncated)]
    #[case::indefinite("3080", BerError::BadLength)]
    #[case::five_length_octets("04850000000001", BerError::BadLength)]
    #[case::long_length_short("048201", BerError::Truncated)]
    #[case::high_tag_number("1f0100", BerError::BadTag(0x1F))]
    fn reader_refuses(#[case] input: &str, #[case] expected: BerError) {
        assert_eq!(Reader::new(&hex(input)).raw_tlv(), Err(expected));
    }

    #[rstest]
    #[case::integer_empty("0200", BerError::BadLength)]
    #[case::integer_nine_octets("0209008000000000000000", BerError::Overflow)]
    fn integer_refuses(#[case] input: &str, #[case] expected: BerError) {
        assert_eq!(Reader::new(&hex(input)).i64(Tag::Integer), Err(expected));
    }

    #[rstest]
    #[case::nine_octets_without_zero_lead("4609018000000000000000", BerError::Overflow)]
    #[case::ten_octets("460a00008000000000000000", BerError::Overflow)]
    fn unsigned_refuses(#[case] input: &str, #[case] expected: BerError) {
        assert_eq!(Reader::new(&hex(input)).u64(Tag::Counter64), Err(expected));
    }

    #[test]
    fn exception_and_null_encodings() {
        let mut out = Vec::new();
        write_null(&mut out);
        write_tlv(&mut out, Tag::NoSuchObject, |_| {});
        write_tlv(&mut out, Tag::NoSuchInstance, |_| {});
        write_tlv(&mut out, Tag::EndOfMibView, |_| {});
        assert_eq!(out, hex("0500 8000 8100 8200"));
    }

    #[test]
    fn message_round_trips_and_sizes() {
        let oid = Oid::parse("1.3.6.1.2.1.1.3.0").expect("valid");
        let mut value = Vec::new();
        write_u64(&mut value, Tag::TimeTicks, 4242);
        let mut vb = Vec::new();
        write_varbind(&mut vb, &oid, &value);
        assert_eq!(vb.len(), varbind_len(&oid, value.len()));
        let mut out = Vec::new();
        write_message(&mut out, 1, b"public", Tag::Response, 77, 0, 0, |out| {
            out.extend_from_slice(&vb)
        });
        assert_eq!(out.len(), message_len(1, b"public", 77, 0, 0, vb.len()));
        let message = Message::decode(&out).expect("decodes");
        assert_eq!(message.version, 1);
        assert_eq!(message.community, b"public");
        assert_eq!(message.pdu, Tag::Response);
        assert_eq!(message.request_id, 77);
        let varbinds: Vec<_> = message
            .varbinds()
            .collect::<Result<_, _>>()
            .expect("varbinds decode");
        assert_eq!(varbinds.len(), 1);
        assert_eq!(varbinds[0].name(), Ok(oid));
        assert_eq!(varbinds[0].value_tag, Tag::TimeTicks.byte());
        assert_eq!(decode_u64(varbinds[0].value), Ok(4242));
    }

    #[test]
    fn message_long_form_lengths_at_every_level() {
        let oid = Oid::parse("1.3.6.1.4.1.8072.9999.1.11.0").expect("valid");
        let mut value = Vec::new();
        write_octets(&mut value, Tag::OctetString, &[b'x'; 300]);
        let mut out = Vec::new();
        write_message(&mut out, 1, b"public", Tag::Response, 1, 0, 0, |out| {
            write_varbind(out, &oid, &value)
        });
        assert_eq!(&out[..2], &[0x30, 0x82]);
        assert_eq!(
            out.len(),
            message_len(1, b"public", 1, 0, 0, varbind_len(&oid, value.len()))
        );
        let message = Message::decode(&out).expect("decodes");
        let varbind = message.varbinds().next().expect("one").expect("decodes");
        assert_eq!(varbind.value.len(), 300);
    }

    #[rstest]
    #[case::trailing_after_message("3000 00", BerError::BadLength)]
    #[case::v3_shape("3009 020103 3000 0400 3000", BerError::BadTag(0x30))]
    #[case::not_a_pdu("300d 020101 0406 7075626c6963 0500", BerError::BadTag(0x05))]
    fn message_refuses(#[case] input: &str, #[case] expected: BerError) {
        assert_eq!(Message::decode(&hex(input)), Err(expected));
    }
    /// Re-encode a value TLV from what it decodes to.
    fn reencode_value(tag: u8, body: &[u8], out: &mut Vec<u8>) {
        match Tag::from_byte(tag).expect("known value tag") {
            Tag::Integer => write_i64(out, Tag::Integer, decode_i64(body).expect("integer")),
            tag @ (Tag::Counter32 | Tag::Gauge32 | Tag::TimeTicks | Tag::Counter64) => {
                write_u64(out, tag, decode_u64(body).expect("unsigned"))
            }
            Tag::OctetString => write_octets(out, Tag::OctetString, body),
            Tag::Oid => write_oid(out, &Oid::from_ber_body(body).expect("oid")),
            Tag::Null => write_null(out),
            tag @ (Tag::NoSuchObject | Tag::NoSuchInstance | Tag::EndOfMibView) => {
                write_tlv(out, tag, |_| {})
            }
            other => panic!("no value re-encoding for {other:?}"),
        }
    }

    /// Decode a captured datagram and write it again from the decoded
    /// fields alone.
    fn reencode_message(datagram: &[u8]) -> Vec<u8> {
        let message = Message::decode(datagram).expect("captured datagram decodes");
        let varbinds: Vec<VarBind<'_>> = message
            .varbinds()
            .collect::<Result<_, _>>()
            .expect("captured varbinds decode");
        let mut out = Vec::new();
        write_message(
            &mut out,
            message.version,
            message.community,
            message.pdu,
            message.request_id,
            message.error_status,
            message.error_index,
            |out| {
                for varbind in &varbinds {
                    let mut value = Vec::new();
                    reencode_value(varbind.value_tag, varbind.value, &mut value);
                    write_varbind(out, &varbind.name().expect("name"), &value);
                }
            },
        );
        out
    }

    #[rustfmt::skip]
    #[rstest]
    #[case::net_snmp_get(            NET_SNMP_GET)]
    #[case::net_snmp_get_reply(      NET_SNMP_GET_REPLY)]
    #[case::net_snmp_getnext(        NET_SNMP_GETNEXT)]
    #[case::net_snmp_getnext_reply(  NET_SNMP_GETNEXT_REPLY)]
    #[case::net_snmp_getbulk(        NET_SNMP_GETBULK)]
    #[case::net_snmp_getbulk_reply(  NET_SNMP_GETBULK_REPLY)]
    #[case::gosnmp_get(              GOSNMP_GET)]
    #[case::gosnmp_getbulk(          GOSNMP_GETBULK)]
    #[case::too_big_reply(           TOO_BIG_REPLY)]
    #[case::net_snmp_trap(           NET_SNMP_TRAP)]
    fn captured_datagram_reencodes_byte_identical(#[case] captured: &str) {
        let bytes = hex(captured);
        assert!(bytes.len() > 20, "fixture present");
        assert_eq!(bytes[0], 0x30, "fixture is a SEQUENCE");
        assert_eq!(reencode_message(&bytes), bytes);
    }

    #[rustfmt::skip]
    #[rstest]
    #[case::net_snmp_get(     NET_SNMP_GET,     Tag::GetRequest,     0x3ac2_5c77, 0, 0,  "1.3.6.1.2.1.1.3.0")]
    #[case::net_snmp_getnext( NET_SNMP_GETNEXT, Tag::GetNextRequest, 0x2db4_14d6, 0, 0,  "1.3.6.1.2.1.31.1.1.1.6")]
    #[case::net_snmp_getbulk( NET_SNMP_GETBULK, Tag::GetBulkRequest, 0x1573_e9b5, 0, 10, "1.3.6.1.2.1.31.1.1.1.6")]
    #[case::gosnmp_get(       GOSNMP_GET,       Tag::GetRequest,     0x4033_a1b4, 0, 0,  "1.3.6.1.2.1.1.3.0")]
    #[case::gosnmp_getbulk(   GOSNMP_GETBULK,   Tag::GetBulkRequest, 0x4033_a1bc, 0, 10, "1.3.6.1.2.1.31.1.1.1.6")]
    fn captured_request_decodes(
        #[case] captured: &str,
        #[case] pdu: Tag,
        #[case] request_id: i64,
        #[case] status_or_non_repeaters: i64,
        #[case] index_or_max_repetitions: i64,
        #[case] name: &str,
    ) {
        let bytes = hex(captured);
        assert_eq!(bytes.first(), Some(&0x30), "fixture present");
        let message = Message::decode(&bytes).expect("decodes");
        assert_eq!(message.version, 1);
        assert_eq!(message.community, b"public");
        assert_eq!(message.pdu, pdu);
        assert_eq!(message.request_id, request_id);
        assert_eq!(message.error_status, status_or_non_repeaters);
        assert_eq!(message.error_index, index_or_max_repetitions);
        let varbinds: Vec<_> = message.varbinds().collect::<Result<_, _>>().expect("varbinds");
        assert_eq!(varbinds.len(), 1);
        assert_eq!(varbinds[0].name(), Ok(Oid::parse(name).expect("valid")));
        assert_eq!((varbinds[0].value_tag, varbinds[0].value), (Tag::Null.byte(), &[][..]));
    }

    #[test]
    fn net_snmp_trap_values_decode_to_what_was_sent() {
        let bytes = hex(NET_SNMP_TRAP);
        assert_eq!(bytes.first(), Some(&0x30), "fixture present");
        let message = Message::decode(&bytes).expect("decodes");
        assert_eq!(message.pdu, Tag::TrapV2);
        let varbinds: Vec<_> = message
            .varbinds()
            .collect::<Result<_, _>>()
            .expect("varbinds");
        // sysUpTime.0 and snmpTrapOID.0, then the twelve playpen values.
        assert_eq!(varbinds.len(), 14);
        let value = |i: usize| (varbinds[i].value_tag, varbinds[i].value);
        let integer = |i: usize| decode_i64(value(i).1).expect("integer");
        let unsigned = |i: usize| decode_u64(value(i).1).expect("unsigned");
        assert_eq!([integer(2), integer(3), integer(4)], [-1, -128, -129]);
        assert_eq!(
            [integer(5), integer(6)],
            [i64::from(i32::MIN), i64::from(i32::MAX)]
        );
        assert_eq!(value(7).0, Tag::Counter64.byte());
        assert_eq!([unsigned(7), unsigned(8)], [1 << 63, u64::MAX]);
        assert_eq!(value(9).0, Tag::Counter32.byte());
        assert_eq!(unsigned(9), u64::from(u32::MAX));
        assert_eq!((value(10).0, unsigned(10)), (Tag::Gauge32.byte(), 0));
        assert_eq!(
            (value(11).0, unsigned(11)),
            (Tag::TimeTicks.byte(), u64::from(u32::MAX))
        );
        assert_eq!(value(12), (Tag::OctetString.byte(), &[b'b'; 128][..]));
        assert_eq!(
            Oid::from_ber_body(value(13).1),
            Some(Oid::parse("1.3.6.1.4.1.4294967295.16384").expect("valid"))
        );
    }
}
