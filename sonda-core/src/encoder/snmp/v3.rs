//! The SNMPv3 message (RFC 3412) with the User-based Security Model's
//! parameters (RFC 3414): decoded borrowing from the datagram, and written
//! into a caller's buffer.
//!
//! ```text
//! Message:   SEQUENCE { version INTEGER (3),
//!                       msgGlobalData SEQUENCE { msgID, msgMaxSize, msgFlags OCTET STRING (1),
//!                                                msgSecurityModel },
//!                       msgSecurityParameters OCTET STRING (UsmSecurityParameters),
//!                       msgData ScopedPDU | OCTET STRING (encrypted ScopedPDU) }
//! USM:       SEQUENCE { engineID OCTET STRING, engineBoots, engineTime, userName OCTET STRING,
//!                       authenticationParameters OCTET STRING, privacyParameters OCTET STRING }
//! ScopedPDU: SEQUENCE { contextEngineID OCTET STRING, contextName OCTET STRING, PDU }
//! ```
//!
//! The PDU inside a scoped PDU is the SNMPv2c PDU, decoded as a
//! [`Message`] whose version is 3 and whose community is empty, so the
//! SNMPv2c handling reads it unchanged.

use std::ops::Range;

use super::ber::{self, BerError, Message, Reader, Tag};
use crate::sink::snmp::usm::{self, LocalizedUser, Snapshot, UsmStat, MAX_AUTH_PARAMS};

/// The version field of an SNMPv3 message.
pub(crate) const VERSION: i64 = 3;

/// `msgSecurityModel` of the User-based Security Model.
pub(crate) const USM: i64 = 3;

/// The smallest `msgMaxSize` (RFC 3412).
pub(crate) const MIN_MSG_MAX_SIZE: i64 = 484;

/// The largest `msgID`, `msgMaxSize`, `msgSecurityModel`, `engineBoots`
/// and `engineTime`: 2³¹ − 1.
const MAX_FIELD: i64 = i32::MAX as i64;

/// The `msgMaxSize` of every message written: the largest UDP payload over
/// IPv4, which the agent reads whole.
const MSG_MAX_SIZE: i64 = 65_507;

/// `msgFlags`' authentication bit.
const AUTH_FLAG: u8 = 0x01;

/// `msgFlags`' privacy bit.
const PRIV_FLAG: u8 = 0x02;

/// `msgFlags`' reportable bit.
const REPORTABLE_FLAG: u8 = 0x04;

/// A message's `msgFlags`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Flags {
    /// The message is authenticated.
    pub(crate) auth: bool,
    /// The scoped PDU is encrypted.
    pub(crate) privacy: bool,
    /// The sender expects a `Report` when the message cannot be processed.
    pub(crate) reportable: bool,
}

impl Flags {
    fn from_octet(octet: u8) -> Self {
        Self {
            auth: octet & AUTH_FLAG != 0,
            privacy: octet & PRIV_FLAG != 0,
            reportable: octet & REPORTABLE_FLAG != 0,
        }
    }

    fn octet(self) -> u8 {
        [
            (self.auth, AUTH_FLAG),
            (self.privacy, PRIV_FLAG),
            (self.reportable, REPORTABLE_FLAG),
        ]
        .into_iter()
        .filter(|(set, _)| *set)
        .fold(0, |octet, (_, flag)| octet | flag)
    }
}

/// A message's `UsmSecurityParameters`, borrowing from the datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UsmParams<'a> {
    /// `msgAuthoritativeEngineID`.
    pub(crate) engine_id: &'a [u8],
    /// `msgAuthoritativeEngineBoots`.
    pub(crate) boots: u32,
    /// `msgAuthoritativeEngineTime`.
    pub(crate) time: u32,
    /// `msgUserName`.
    pub(crate) user: &'a [u8],
    /// `msgAuthenticationParameters`: the HMAC, or empty.
    pub(crate) auth_params: &'a [u8],
    /// `msgPrivacyParameters`: the salt, or empty.
    pub(crate) priv_params: &'a [u8],
}

/// A plaintext scoped PDU, borrowing from the datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScopedPdu<'a> {
    pub(crate) context_engine_id: &'a [u8],
    pub(crate) context_name: &'a [u8],
    /// The PDU, as a [`Message`] of version 3 with an empty community.
    pub(crate) pdu: Message<'a>,
}

/// A decoded SNMPv3 message, borrowing from the datagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct V3Message<'a> {
    /// `msgID`, echoed in the response.
    pub(crate) msg_id: i64,
    /// `msgMaxSize`: the largest message the sender accepts, at least 484.
    pub(crate) msg_max_size: usize,
    pub(crate) flags: Flags,
    /// `msgSecurityModel`; the USM's is 3.
    pub(crate) security_model: i64,
    pub(crate) usm: UsmParams<'a>,
    /// Where `usm.auth_params` is in the datagram.
    pub(crate) auth_range: Range<usize>,
    /// The scoped PDU, or `None` when the privacy flag says it is encrypted.
    pub(crate) scoped: Option<ScopedPdu<'a>>,
}

/// The next TLV of `reader`, an INTEGER, when it is 0 to 2³¹ − 1 and at
/// least `min`; [`BerError::Overflow`] otherwise.
fn field(reader: &mut Reader<'_>, min: i64) -> Result<i64, BerError> {
    let value = reader.i64(Tag::Integer)?;
    if !(min..=MAX_FIELD).contains(&value) {
        return Err(BerError::Overflow);
    }
    Ok(value)
}

/// Decode an SNMPv3 message.
///
/// The version is the first field read: any version but 3 is
/// [`BerError::UnsupportedVersion`]. The message must be a well-formed
/// SNMPv3 message with USM security parameters: `msgID`, `msgSecurityModel`,
/// `engineBoots` and `engineTime` 0 to 2³¹ − 1 and `msgMaxSize` 484 to
/// 2³¹ − 1, else [`BerError::Overflow`]; `msgFlags` one octet, else
/// [`BerError::BadLength`]; with the privacy flag the data is an OCTET
/// STRING, which is not decrypted, otherwise a scoped PDU whose PDU is one
/// of the SNMPv2c PDUs. Bytes after any SEQUENCE's last field are
/// [`BerError::BadLength`].
pub(crate) fn decode(datagram: &[u8]) -> Result<V3Message<'_>, BerError> {
    let mut outer = Reader::new(datagram);
    let body = outer.expect(Tag::Sequence)?;
    if !outer.is_empty() {
        return Err(BerError::BadLength);
    }
    let mut message = Reader::new(body);
    let version = message.i64(Tag::Integer)?;
    if version != VERSION {
        return Err(BerError::UnsupportedVersion(version));
    }
    let mut global = Reader::new(message.expect(Tag::Sequence)?);
    let msg_id = field(&mut global, 0)?;
    let msg_max_size = field(&mut global, MIN_MSG_MAX_SIZE)?;
    let flags = match global.octets(Tag::OctetString)? {
        [octet] => Flags::from_octet(*octet),
        _ => return Err(BerError::BadLength),
    };
    let security_model = field(&mut global, 0)?;
    if !global.is_empty() {
        return Err(BerError::BadLength);
    }
    let mut security = Reader::new(message.octets(Tag::OctetString)?);
    let mut params = Reader::new(security.expect(Tag::Sequence)?);
    if !security.is_empty() {
        return Err(BerError::BadLength);
    }
    let engine_id = params.octets(Tag::OctetString)?;
    let boots = field(&mut params, 0)?;
    let time = field(&mut params, 0)?;
    let user = params.octets(Tag::OctetString)?;
    let auth_params = params.octets(Tag::OctetString)?;
    let priv_params = params.octets(Tag::OctetString)?;
    if !params.is_empty() {
        return Err(BerError::BadLength);
    }
    // `auth_params` is a subslice of `datagram`.
    let auth_at = auth_params.as_ptr() as usize - datagram.as_ptr() as usize;
    let scoped = if flags.privacy {
        message.octets(Tag::OctetString)?;
        None
    } else {
        Some(scoped_pdu(message.expect(Tag::Sequence)?)?)
    };
    if !message.is_empty() {
        return Err(BerError::BadLength);
    }
    Ok(V3Message {
        msg_id,
        // Both bounds were checked above.
        msg_max_size: usize::try_from(msg_max_size).unwrap_or(usize::MAX),
        flags,
        security_model,
        usm: UsmParams {
            engine_id,
            boots: u32::try_from(boots).unwrap_or(u32::MAX),
            time: u32::try_from(time).unwrap_or(u32::MAX),
            user,
            auth_params,
            priv_params,
        },
        auth_range: auth_at..auth_at + auth_params.len(),
        scoped,
    })
}

/// Decode the body of a scoped PDU.
fn scoped_pdu(body: &[u8]) -> Result<ScopedPdu<'_>, BerError> {
    let mut scoped = Reader::new(body);
    let context_engine_id = scoped.octets(Tag::OctetString)?;
    let context_name = scoped.octets(Tag::OctetString)?;
    let (pdu, pdu_body) = scoped.tlv()?;
    if !pdu.is_pdu() || !scoped.is_empty() {
        return Err(if pdu.is_pdu() {
            BerError::BadLength
        } else {
            BerError::BadTag(pdu.byte())
        });
    }
    let mut fields = Reader::new(pdu_body);
    let request_id = fields.i64(Tag::Integer)?;
    let error_status = fields.i64(Tag::Integer)?;
    let error_index = fields.i64(Tag::Integer)?;
    let varbinds = fields.expect(Tag::Sequence)?;
    if !fields.is_empty() {
        return Err(BerError::BadLength);
    }
    Ok(ScopedPdu {
        context_engine_id,
        context_name,
        pdu: Message {
            version: VERSION,
            community: &[],
            pdu,
            request_id,
            error_status,
            error_index,
            varbinds,
        },
    })
}

/// The authentication parameter's length for a message authenticated by
/// `user`: 0 when it is not.
fn auth_len(user: Option<&LocalizedUser>) -> usize {
    user.and_then(LocalizedUser::auth)
        .map_or(0, |(protocol, _)| usm::auth_params_len(protocol))
}

/// Octets the body of the USM security parameters' SEQUENCE takes.
fn usm_body_len(engine: &Snapshot<'_>, user: &[u8], auth_len: usize) -> usize {
    ber::tlv_len(engine.id.len())
        + ber::i64_len(i64::from(engine.boots))
        + ber::i64_len(i64::from(engine.time))
        + ber::tlv_len(user.len())
        + ber::tlv_len(auth_len)
        + ber::tlv_len(0)
}

/// Octets the message answering `req` takes whose PDU TLV is `pdu_len`
/// octets, authenticated by `user` or not, written with `engine`: what
/// [`write_response`] writes.
pub(crate) fn response_len(
    req: &V3Message<'_>,
    engine: &Snapshot<'_>,
    user: Option<&LocalizedUser>,
    pdu_len: usize,
) -> usize {
    let global = ber::tlv_len(
        ber::i64_len(req.msg_id) + ber::i64_len(MSG_MAX_SIZE) + ber::tlv_len(1) + ber::i64_len(USM),
    );
    let security = ber::tlv_len(ber::tlv_len(usm_body_len(
        engine,
        req.usm.user,
        auth_len(user),
    )));
    let scoped = ber::tlv_len(ber::tlv_len(engine.id.len()) + ber::tlv_len(0) + pdu_len);
    ber::tlv_len(ber::i64_len(VERSION) + global + security + scoped)
}

/// Write the message answering `req` whose PDU `scoped_pdu` appends: at
/// `user`'s security level when it is given (authenticated with its key),
/// otherwise `noAuthNoPriv`; `reportable` clear; `msgID` and the user name
/// echoed; the engine's ID, boots and time from `engine`; the scoped PDU's
/// context engine ID the engine's and its context name empty. The
/// authentication parameter is written zeroed, then replaced by the HMAC of
/// the whole message.
pub(crate) fn write_response(
    out: &mut Vec<u8>,
    req: &V3Message<'_>,
    engine: &Snapshot<'_>,
    user: Option<&LocalizedUser>,
    scoped_pdu: impl FnOnce(&mut Vec<u8>),
) {
    let start = out.len();
    let auth_len = auth_len(user);
    let flags = Flags {
        auth: auth_len > 0,
        privacy: false,
        reportable: false,
    };
    // Where the authentication parameter is, from the start of the outer
    // SEQUENCE's body: nothing before it moves once it is written but that
    // body, as a whole, when its length takes the long form.
    let mut auth_at = 0;
    ber::write_tlv(out, Tag::Sequence, |out| {
        let body = out.len();
        ber::write_i64(out, Tag::Integer, VERSION);
        ber::write_tlv(out, Tag::Sequence, |out| {
            ber::write_i64(out, Tag::Integer, req.msg_id);
            ber::write_i64(out, Tag::Integer, MSG_MAX_SIZE);
            ber::write_octets(out, Tag::OctetString, &[flags.octet()]);
            ber::write_i64(out, Tag::Integer, USM);
        });
        let params = usm_body_len(engine, req.usm.user, auth_len);
        ber::write_header(out, Tag::OctetString, ber::tlv_len(params));
        ber::write_header(out, Tag::Sequence, params);
        ber::write_octets(out, Tag::OctetString, engine.id);
        ber::write_i64(out, Tag::Integer, i64::from(engine.boots));
        ber::write_i64(out, Tag::Integer, i64::from(engine.time));
        ber::write_octets(out, Tag::OctetString, req.usm.user);
        ber::write_header(out, Tag::OctetString, auth_len);
        auth_at = out.len() - body;
        out.resize(out.len() + auth_len, 0);
        ber::write_octets(out, Tag::OctetString, &[]);
        ber::write_tlv(out, Tag::Sequence, |out| {
            ber::write_octets(out, Tag::OctetString, engine.id);
            ber::write_octets(out, Tag::OctetString, &[]);
            scoped_pdu(out);
        });
    });
    let Some((protocol, key)) = user.and_then(LocalizedUser::auth) else {
        return;
    };
    let at = body_start(out, start) + auth_at;
    let mut digest = [0u8; MAX_AUTH_PARAMS];
    usm::hmac(protocol, key, &out[start..], &mut digest[..auth_len]);
    out[at..at + auth_len].copy_from_slice(&digest[..auth_len]);
}

/// Write the `Report` answering `req` with the usmStats counter `stat` at
/// `value`, as [`write_response`] writes a response: one varbind, the
/// counter's instance as a Counter32; the request-id the request's, or 0
/// when its scoped PDU is encrypted.
pub(crate) fn write_report(
    out: &mut Vec<u8>,
    req: &V3Message<'_>,
    engine: &Snapshot<'_>,
    stat: UsmStat,
    value: u32,
    user: Option<&LocalizedUser>,
) {
    let request_id = req.scoped.map_or(0, |scoped| scoped.pdu.request_id);
    write_response(out, req, engine, user, |out| {
        ber::write_tlv(out, Tag::Report, |out| {
            ber::write_i64(out, Tag::Integer, request_id);
            ber::write_i64(out, Tag::Integer, 0);
            ber::write_i64(out, Tag::Integer, 0);
            ber::write_tlv(out, Tag::Sequence, |out| {
                ber::write_tlv(out, Tag::Sequence, |out| {
                    ber::write_octets(out, Tag::Oid, stat.oid_body());
                    ber::write_counter32(out, value);
                });
            });
        });
    });
}

/// Where the body of the TLV that starts at `start` in `out` starts.
fn body_start(out: &[u8], start: usize) -> usize {
    match out.get(start + 1) {
        Some(&len) if len & 0x80 != 0 => start + 2 + usize::from(len & 0x7f),
        _ => start + 2,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use rstest::rstest;

    use super::*;
    use crate::sink::snmp::{AuthProtocol, UsmAuth, UsmUser};

    pub(crate) fn hex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
        digits
            .chunks(2)
            .map(|pair| {
                let pair = std::str::from_utf8(pair).expect("hex digits are ASCII");
                u8::from_str_radix(pair, 16).expect("hex digits")
            })
            .collect()
    }

    /// A manager building SNMPv3 requests by hand: its view of the agent's
    /// engine, its user and, when it authenticates, its localized key.
    #[derive(Debug, Clone)]
    pub(crate) struct Client {
        pub(crate) engine_id: Vec<u8>,
        pub(crate) boots: u32,
        pub(crate) time: u32,
        pub(crate) user: Vec<u8>,
        pub(crate) key: Option<(AuthProtocol, Vec<u8>)>,
        /// Set the privacy flag and send the scoped PDU as an OCTET STRING.
        pub(crate) privacy: bool,
        pub(crate) msg_max_size: i64,
    }

    impl Client {
        /// A client of `user` that has not discovered the engine, with the
        /// localized `key` when it authenticates.
        pub(crate) fn new(user: &str, key: Option<(AuthProtocol, Vec<u8>)>) -> Self {
            Self {
                engine_id: Vec::new(),
                boots: 0,
                time: 0,
                user: user.as_bytes().to_vec(),
                key,
                privacy: false,
                msg_max_size: 65_507,
            }
        }

        /// A reportable `GetRequest` of `names`, request-id and msgID `id`.
        pub(crate) fn get(&mut self, id: i64, names: &[&str]) -> Vec<u8> {
            let len = self
                .key
                .as_ref()
                .map_or(0, |(p, _)| usm::auth_params_len(*p));
            self.request(Tag::GetRequest, id, (0, 0), names, len)
        }

        /// [`get`](Self::get) with an authentication parameter of `len`
        /// octets.
        pub(crate) fn get_with_auth_len(&mut self, id: i64, names: &[&str], len: usize) -> Vec<u8> {
            self.request(Tag::GetRequest, id, (0, 0), names, len)
        }

        /// A reportable request of `pdu` with the two fields after the
        /// request-id, carrying `names` with NULL values, authenticated
        /// with a parameter of `auth_len` octets when the client has a key.
        pub(crate) fn request(
            &mut self,
            pdu: Tag,
            id: i64,
            fields: (i64, i64),
            names: &[&str],
            auth_len: usize,
        ) -> Vec<u8> {
            let flags = Flags {
                auth: self.key.is_some(),
                privacy: self.privacy,
                reportable: true,
            };
            let mut scoped = Vec::new();
            ber::write_tlv(&mut scoped, Tag::Sequence, |out| {
                ber::write_octets(out, Tag::OctetString, &self.engine_id);
                ber::write_octets(out, Tag::OctetString, &[]);
                ber::write_tlv(out, pdu, |out| {
                    ber::write_i64(out, Tag::Integer, id);
                    ber::write_i64(out, Tag::Integer, fields.0);
                    ber::write_i64(out, Tag::Integer, fields.1);
                    ber::write_tlv(out, Tag::Sequence, |out| {
                        for name in names {
                            let oid = crate::encoder::snmp::oid::Oid::parse(name).expect("an OID");
                            let mut null = Vec::new();
                            ber::write_null(&mut null);
                            ber::write_varbind(out, &oid, &null);
                        }
                    });
                });
            });
            let mut params = Vec::new();
            ber::write_tlv(&mut params, Tag::Sequence, |out| {
                ber::write_octets(out, Tag::OctetString, &self.engine_id);
                ber::write_i64(out, Tag::Integer, i64::from(self.boots));
                ber::write_i64(out, Tag::Integer, i64::from(self.time));
                ber::write_octets(out, Tag::OctetString, &self.user);
                let zeros = vec![0; if self.key.is_some() { auth_len } else { 0 }];
                ber::write_octets(out, Tag::OctetString, &zeros);
                ber::write_octets(
                    out,
                    Tag::OctetString,
                    if self.privacy { &[0; 8] } else { &[] },
                );
            });
            let mut datagram = Vec::new();
            ber::write_tlv(&mut datagram, Tag::Sequence, |out| {
                ber::write_i64(out, Tag::Integer, VERSION);
                ber::write_tlv(out, Tag::Sequence, |out| {
                    ber::write_i64(out, Tag::Integer, id);
                    ber::write_i64(out, Tag::Integer, self.msg_max_size);
                    ber::write_octets(out, Tag::OctetString, &[flags.octet()]);
                    ber::write_i64(out, Tag::Integer, USM);
                });
                ber::write_octets(out, Tag::OctetString, &params);
                if self.privacy {
                    ber::write_octets(out, Tag::OctetString, &scoped);
                } else {
                    out.extend_from_slice(&scoped);
                }
            });
            self.sign(&mut datagram);
            datagram
        }

        /// Replace `datagram`'s authentication parameter with the HMAC of
        /// the datagram with it zeroed, when the client has a key.
        pub(crate) fn sign(&self, datagram: &mut [u8]) {
            if let Some((protocol, key)) = &self.key {
                let range = decode(datagram).expect("the request decodes").auth_range;
                datagram[range.clone()].fill(0);
                let mut digest = vec![0; range.len()];
                usm::hmac(*protocol, key, datagram, &mut digest);
                datagram[range].copy_from_slice(&digest);
            }
        }

        /// Take the engine's ID, boots and time from a report.
        pub(crate) fn synchronize(&mut self, report: &[u8]) {
            let report = decode(report).expect("a report decodes");
            self.engine_id = report.usm.engine_id.to_vec();
            self.boots = report.usm.boots;
            self.time = report.usm.time;
        }
    }

    /// `snmpget -v3 -l authNoPriv -u telegraf -a SHA-256 -A lab-sha256-pass -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0`, net-snmp 5.9.4.pre2: its engine-discovery request.
    pub(crate) const NET_SNMP_V3_SHA256_DISCOVERY: &str =
        "303e02010330110204591cbfc1020300ffe30401040201030410300e040002010002010004000400\
        0400301404000400a00e02041c6d35fa0201000201003000";

    /// This codec's report to [`NET_SNMP_V3_SHA256_DISCOVERY`], engine time 3, `usmStatsUnknownEngineIDs` 1, after which snmpget sent [`NET_SNMP_V3_SHA256_GET`].
    pub(crate) const NET_SNMP_V3_SHA256_DISCOVERY_REPORT: &str =
        "307502010330110204591cbfc1020300ffe304010002010304233021041380001f8804736f6e6461\
        3a63617074757265300201010201030400040004003038041380001f8804736f6e64613a63617074\
        757265300400a81f02041c6d35fa0201000201003011300f060a2b060106030f01010400410101";

    /// `snmpget -v3 -l authNoPriv -u telegraf -a SHA-256 -A lab-sha256-pass -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0`, net-snmp 5.9.4.pre2: its authenticated request, engine time 3.
    pub(crate) const NET_SNMP_V3_SHA256_GET: &str =
        "30819202010330110204591cbfc0020300ffe304010502010304433041041380001f8804736f6e64\
        613a6361707475726530020101020103040874656c65677261660418e4f699e21b75d8bb15b6daca\
        92639114529613daa782aa9204003035041380001f8804736f6e64613a63617074757265300400a0\
        1c02041c6d35f9020100020100300e300c06082b060102010105000500";

    /// This codec's response to [`NET_SNMP_V3_SHA256_GET`], which snmpget printed as `.1.3.6.1.2.1.1.5.0 = STRING: "rtr-core-01"`.
    pub(crate) const NET_SNMP_V3_SHA256_RESPONSE: &str =
        "30819d02010330110204591cbfc0020300ffe304010102010304433041041380001f8804736f6e64\
        613a6361707475726530020101020103040874656c656772616604181773743d52b94e56b425f350\
        ef6a7c08cb34048b9771243a04003040041380001f8804736f6e64613a63617074757265300400a2\
        2702041c6d35f90201000201003019301706082b06010201010500040b7274722d636f72652d3031";

    /// `snmpget -v3 -l authNoPriv -u md5user -a MD5 -A lab-md5-pass -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0`, net-snmp 5.9.4.pre2: its authenticated request, engine time 5.
    pub(crate) const NET_SNMP_V3_MD5_GET: &str =
        "308185020103301102045294c247020300ffe304010502010304363034041380001f8804736f6e64\
        613a636170747572653002010102010504076d643575736572040c99ee10ae9519109b2b4d20bf04\
        003035041380001f8804736f6e64613a63617074757265300400a01c0204128c329c020100020100\
        300e300c06082b060102010105000500";

    /// This codec's response to [`NET_SNMP_V3_MD5_GET`], which snmpget printed as `.1.3.6.1.2.1.1.5.0 = STRING: "rtr-core-01"`.
    pub(crate) const NET_SNMP_V3_MD5_RESPONSE: &str =
        "308190020103301102045294c247020300ffe304010102010304363034041380001f8804736f6e64\
        613a636170747572653002010102010504076d643575736572040c39da5f30b197feb7c1d4fa5b04\
        003040041380001f8804736f6e64613a63617074757265300400a2270204128c329c020100020100\
        3019301706082b06010201010500040b7274722d636f72652d3031";

    /// `snmpget -v3 -l authNoPriv -u shauser -a SHA -A lab-sha1-pass -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0`, net-snmp 5.9.4.pre2: its authenticated request, engine time 7.
    pub(crate) const NET_SNMP_V3_SHA1_GET: &str =
        "308185020103301102041ec1ba03020300ffe304010502010304363034041380001f8804736f6e64\
        613a6361707475726530020101020107040773686175736572040c7d32d4c7e2e2ab251a4b602a04\
        003035041380001f8804736f6e64613a63617074757265300400a01c02047ee4db33020100020100\
        300e300c06082b060102010105000500";

    /// This codec's response to [`NET_SNMP_V3_SHA1_GET`], which snmpget printed as `.1.3.6.1.2.1.1.5.0 = STRING: "rtr-core-01"`.
    pub(crate) const NET_SNMP_V3_SHA1_RESPONSE: &str =
        "308190020103301102041ec1ba03020300ffe304010102010304363034041380001f8804736f6e64\
        613a6361707475726530020101020107040773686175736572040c338b24ad826a457c12d0cd5604\
        003040041380001f8804736f6e64613a63617074757265300400a22702047ee4db33020100020100\
        3019301706082b06010201010500040b7274722d636f72652d3031";

    /// `snmpget -v3 -l authNoPriv -u sha224user -a SHA-224 -A lab-sha224-pass -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0`, net-snmp 5.9.4.pre2: its authenticated request, engine time 9.
    pub(crate) const NET_SNMP_V3_SHA224_GET: &str =
        "30818c0201033011020440ff5ba0020300ffe3040105020103043d303b041380001f8804736f6e64\
        613a6361707475726530020101020109040a7368613232347573657204106bd79066502758b0345d\
        1669a685397004003035041380001f8804736f6e64613a63617074757265300400a01c02046a5367\
        a6020100020100300e300c06082b060102010105000500";

    /// This codec's response to [`NET_SNMP_V3_SHA224_GET`], which snmpget printed as `.1.3.6.1.2.1.1.5.0 = STRING: "rtr-core-01"`.
    pub(crate) const NET_SNMP_V3_SHA224_RESPONSE: &str =
        "3081970201033011020440ff5ba0020300ffe3040101020103043d303b041380001f8804736f6e64\
        613a6361707475726530020101020109040a73686132323475736572041067d9ee321b151492dd49\
        5951f707371004003040041380001f8804736f6e64613a63617074757265300400a22702046a5367\
        a60201000201003019301706082b06010201010500040b7274722d636f72652d3031";

    /// `snmpget -v3 -l authNoPriv -u sha384user -a SHA-384 -A lab-sha384-pass -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0`, net-snmp 5.9.4.pre2: its authenticated request, engine time 11.
    pub(crate) const NET_SNMP_V3_SHA384_GET: &str =
        "30819c0201033011020421f440ea020300ffe3040105020103044d304b041380001f8804736f6e64\
        613a636170747572653002010102010b040a7368613338347573657204205a7fbd93ad4e3578aff7\
        6f0b7d21776480fe025d4c91003de7f4418fba2889ca04003035041380001f8804736f6e64613a63\
        617074757265300400a01c02045317a387020100020100300e300c06082b060102010105000500";

    /// This codec's response to [`NET_SNMP_V3_SHA384_GET`], which snmpget printed as `.1.3.6.1.2.1.1.5.0 = STRING: "rtr-core-01"`.
    pub(crate) const NET_SNMP_V3_SHA384_RESPONSE: &str =
        "3081a70201033011020421f440ea020300ffe3040101020103044d304b041380001f8804736f6e64\
        613a636170747572653002010102010b040a736861333834757365720420c7a7d116aa6a3d32936e\
        45aaf13c6776a46d6e0257bd57dc7ad4262462f764de04003040041380001f8804736f6e64613a63\
        617074757265300400a22702045317a3870201000201003019301706082b06010201010500040b72\
        74722d636f72652d3031";

    /// `snmpget -v3 -l authNoPriv -u sha512user -a SHA-512 -A lab-sha512-pass -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0`, net-snmp 5.9.4.pre2: its authenticated request, engine time 13.
    pub(crate) const NET_SNMP_V3_SHA512_GET: &str =
        "3081ac020103301102047ac61dfc020300ffe3040105020103045d305b041380001f8804736f6e64\
        613a636170747572653002010102010d040a736861353132757365720430cef097cda2ca6edad255\
        7b82131461256daf0439ff0da4ef66190d66da8bf4c9d1278042f8768936641bdba68ee08d380400\
        3035041380001f8804736f6e64613a63617074757265300400a01c0204197ae98602010002010030\
        0e300c06082b060102010105000500";

    /// This codec's response to [`NET_SNMP_V3_SHA512_GET`], which snmpget printed as `.1.3.6.1.2.1.1.5.0 = STRING: "rtr-core-01"`.
    pub(crate) const NET_SNMP_V3_SHA512_RESPONSE: &str =
        "3081b7020103301102047ac61dfc020300ffe3040101020103045d305b041380001f8804736f6e64\
        613a636170747572653002010102010d040a736861353132757365720430f818b43049910dd8d1db\
        d4dd57855c3d954ef978bea836ca8a0ebe73ff99eb0690646ab7ed413ceeeee3c1701ae2b1de0400\
        3040041380001f8804736f6e64613a63617074757265300400a2270204197ae98602010002010030\
        19301706082b06010201010500040b7274722d636f72652d3031";

    /// `snmpget -v3 -l authNoPriv -u telegraf -a SHA-256 -A wrong-password -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0`, net-snmp 5.9.4.pre2: its authenticated request, engine time 15.
    pub(crate) const NET_SNMP_V3_WRONG_PASSWORD_GET: &str =
        "3081920201033011020402210560020300ffe304010502010304433041041380001f8804736f6e64\
        613a636170747572653002010102010f040874656c6567726166041833b66296b6796dff36b388a2\
        b7947ec9e5d65701a38a60a004003035041380001f8804736f6e64613a63617074757265300400a0\
        1c02046876f45f020100020100300e300c06082b060102010105000500";

    /// This codec's report to [`NET_SNMP_V3_WRONG_PASSWORD_GET`], `usmStatsWrongDigests` 1, which snmpget printed as `Authentication failure (incorrect password, community or key)`.
    pub(crate) const NET_SNMP_V3_WRONG_PASSWORD_REPORT: &str =
        "307d0201033011020402210560020300ffe3040100020103042b3029041380001f8804736f6e6461\
        3a636170747572653002010102010f040874656c6567726166040004003038041380001f8804736f\
        6e64613a63617074757265300400a81f02046876f45f0201000201003011300f060a2b060106030f\
        01010500410101";

    /// `snmpget -v3 -l noAuthNoPriv -u watcher -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0`, net-snmp 5.9.4.pre2: its request, engine time 17.
    pub(crate) const NET_SNMP_V3_NO_AUTH_GET: &str =
        "30790201033011020429f22854020300ffe3040104020103042a3028041380001f8804736f6e6461\
        3a6361707475726530020101020111040777617463686572040004003035041380001f8804736f6e\
        64613a63617074757265300400a01c020451352f0f020100020100300e300c06082b060102010105\
        000500";

    /// This codec's response to [`NET_SNMP_V3_NO_AUTH_GET`], which snmpget printed as `.1.3.6.1.2.1.1.5.0 = STRING: "rtr-core-01"`.
    pub(crate) const NET_SNMP_V3_NO_AUTH_RESPONSE: &str =
        "3081840201033011020429f22854020300ffe3040100020103042a3028041380001f8804736f6e64\
        613a6361707475726530020101020111040777617463686572040004003040041380001f8804736f\
        6e64613a63617074757265300400a227020451352f0f0201000201003019301706082b0601020101\
        0500040b7274722d636f72652d3031";

    /// The engine ID every capture's agent was configured with.
    pub(crate) const CAPTURE_ENGINE_ID: &str = "80001f8804736f6e64613a6361707475726530";

    /// One captured net-snmp exchange: its user, the protocol and password
    /// when it authenticates, the request, this codec's response, and the
    /// engine time it was made at.
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct Capture {
        pub(crate) user: &'static str,
        pub(crate) auth: Option<(AuthProtocol, &'static str)>,
        pub(crate) request: &'static str,
        pub(crate) response: &'static str,
        pub(crate) time: u32,
    }

    /// Each authenticated capture and the noAuthNoPriv one, in the order
    /// they were made.
    pub(crate) const CAPTURED: [Capture; 7] = [
        Capture {
            user: "telegraf",
            auth: Some((AuthProtocol::Sha256, "lab-sha256-pass")),
            request: NET_SNMP_V3_SHA256_GET,
            response: NET_SNMP_V3_SHA256_RESPONSE,
            time: 3,
        },
        Capture {
            user: "md5user",
            auth: Some((AuthProtocol::Md5, "lab-md5-pass")),
            request: NET_SNMP_V3_MD5_GET,
            response: NET_SNMP_V3_MD5_RESPONSE,
            time: 5,
        },
        Capture {
            user: "shauser",
            auth: Some((AuthProtocol::Sha1, "lab-sha1-pass")),
            request: NET_SNMP_V3_SHA1_GET,
            response: NET_SNMP_V3_SHA1_RESPONSE,
            time: 7,
        },
        Capture {
            user: "sha224user",
            auth: Some((AuthProtocol::Sha224, "lab-sha224-pass")),
            request: NET_SNMP_V3_SHA224_GET,
            response: NET_SNMP_V3_SHA224_RESPONSE,
            time: 9,
        },
        Capture {
            user: "sha384user",
            auth: Some((AuthProtocol::Sha384, "lab-sha384-pass")),
            request: NET_SNMP_V3_SHA384_GET,
            response: NET_SNMP_V3_SHA384_RESPONSE,
            time: 11,
        },
        Capture {
            user: "sha512user",
            auth: Some((AuthProtocol::Sha512, "lab-sha512-pass")),
            request: NET_SNMP_V3_SHA512_GET,
            response: NET_SNMP_V3_SHA512_RESPONSE,
            time: 13,
        },
        Capture {
            user: "watcher",
            auth: None,
            request: NET_SNMP_V3_NO_AUTH_GET,
            response: NET_SNMP_V3_NO_AUTH_RESPONSE,
            time: 17,
        },
    ];

    /// The users of the agent the captures were made against.
    pub(crate) fn capture_users() -> Vec<UsmUser> {
        CAPTURED
            .iter()
            .map(
                |Capture {
                     user: name, auth, ..
                 }| UsmUser {
                    name: name.to_string(),
                    auth: auth.map(|(protocol, password)| UsmAuth {
                        protocol,
                        password: password.to_string(),
                    }),
                },
            )
            .collect()
    }

    /// `datagram`'s authentication parameter recomputed: the HMAC of the
    /// whole message with the parameter zeroed, under `protocol` with the
    /// key `password` localizes to the capture's engine.
    fn recomputed(datagram: &[u8], protocol: AuthProtocol, password: &str) -> Vec<u8> {
        let range = decode(datagram).expect("decodes").auth_range;
        let mut zeroed = datagram.to_vec();
        zeroed[range.clone()].fill(0);
        let key = usm::localize(protocol, password.as_bytes(), &hex(CAPTURE_ENGINE_ID));
        let mut digest = vec![0; range.len()];
        usm::hmac(protocol, &key, &zeroed, &mut digest);
        digest
    }

    /// net-snmp's discovery request decodes to an empty engine ID and user,
    /// reportable, at noAuthNoPriv, with an empty GET.
    #[test]
    fn a_captured_discovery_request_decodes() {
        let datagram = hex(NET_SNMP_V3_SHA256_DISCOVERY);
        let msg = decode(&datagram).expect("decodes");
        assert_eq!((msg.msg_id, msg.msg_max_size), (0x591c_bfc1, 65_507));
        assert_eq!(
            msg.flags,
            Flags {
                auth: false,
                privacy: false,
                reportable: true
            }
        );
        assert_eq!(msg.security_model, USM);
        assert_eq!(
            msg.usm,
            UsmParams {
                engine_id: b"",
                boots: 0,
                time: 0,
                user: b"",
                auth_params: b"",
                priv_params: b""
            }
        );
        let scoped = msg.scoped.expect("plaintext");
        assert_eq!(
            (scoped.context_engine_id, scoped.context_name),
            (&b""[..], &b""[..])
        );
        assert_eq!(
            (scoped.pdu.pdu, scoped.pdu.request_id),
            (Tag::GetRequest, 0x1c6d_35fa)
        );
        assert!(scoped.pdu.varbinds.is_empty());
    }

    /// net-snmp's authenticated request decodes to the capture's engine, its
    /// boots and time, its user and a 24-octet SHA-256 digest whose range
    /// in the datagram is the digest.
    #[test]
    fn a_captured_authenticated_request_decodes() {
        let datagram = hex(NET_SNMP_V3_SHA256_GET);
        let msg = decode(&datagram).expect("decodes");
        assert_eq!(
            msg.flags,
            Flags {
                auth: true,
                privacy: false,
                reportable: true
            }
        );
        assert_eq!(msg.usm.engine_id, hex(CAPTURE_ENGINE_ID).as_slice());
        assert_eq!(
            (msg.usm.boots, msg.usm.time, msg.usm.user),
            (1, 3, &b"telegraf"[..])
        );
        assert_eq!(msg.usm.auth_params.len(), 24);
        assert_eq!(&datagram[msg.auth_range.clone()], msg.usm.auth_params);
        let scoped = msg.scoped.expect("plaintext");
        assert_eq!(scoped.context_engine_id, hex(CAPTURE_ENGINE_ID).as_slice());
        assert_eq!(
            scoped.pdu.varbinds.len(),
            14,
            "one varbind, sysName.0 with NULL"
        );
    }

    /// The report to net-snmp's discovery is the one snmpget accepted: it
    /// went on to send its authenticated request in the same capture.
    #[test]
    fn the_discovery_report_is_the_one_net_snmp_accepted() {
        let request = hex(NET_SNMP_V3_SHA256_DISCOVERY);
        let msg = decode(&request).expect("decodes");
        let engine = hex(CAPTURE_ENGINE_ID);
        let snapshot = Snapshot {
            id: &engine,
            boots: 1,
            time: 3,
        };
        let mut out = Vec::new();
        write_report(
            &mut out,
            &msg,
            &snapshot,
            UsmStat::UnknownEngineIds,
            1,
            None,
        );
        assert_eq!(out, hex(NET_SNMP_V3_SHA256_DISCOVERY_REPORT));
        // What snmpget learnt from it is what it sent next.
        let get = hex(NET_SNMP_V3_SHA256_GET);
        let next = decode(&get).expect("decodes").usm;
        let report = decode(&out).expect("the report decodes");
        assert_eq!(
            (report.usm.engine_id, report.usm.boots, report.usm.time),
            (next.engine_id, next.boots, next.time)
        );
        assert_eq!(
            report.flags,
            Flags::default(),
            "unauthenticated, not reportable"
        );
    }

    /// The wrong-digest report to net-snmp's request is unauthenticated and
    /// is the one snmpget printed as an authentication failure.
    #[test]
    fn the_wrong_digest_report_is_the_one_net_snmp_printed() {
        let request = hex(NET_SNMP_V3_WRONG_PASSWORD_GET);
        let msg = decode(&request).expect("decodes");
        let engine = hex(CAPTURE_ENGINE_ID);
        let snapshot = Snapshot {
            id: &engine,
            boots: 1,
            time: 15,
        };
        let mut out = Vec::new();
        write_report(&mut out, &msg, &snapshot, UsmStat::WrongDigests, 1, None);
        assert_eq!(out, hex(NET_SNMP_V3_WRONG_PASSWORD_REPORT));
        // Guard: the request's digest is not the right password's.
        assert_ne!(
            recomputed(&request, AuthProtocol::Sha256, "lab-sha256-pass"),
            msg.usm.auth_params
        );
    }

    /// For every protocol, the digest net-snmp put on its request is the one
    /// this module's localization and HMAC make from the password, and the
    /// digest on the response net-snmp accepted is the response's HMAC with
    /// the same key.
    #[rstest]
    #[case::md5(1)]
    #[case::sha1(2)]
    #[case::sha224(3)]
    #[case::sha256(0)]
    #[case::sha384(4)]
    #[case::sha512(5)]
    fn each_protocol_reproduces_net_snmps_digest(#[case] capture: usize) {
        let Capture {
            auth,
            request,
            response,
            ..
        } = CAPTURED[capture];
        let (protocol, password) = auth.expect("an authenticated capture");
        for datagram in [hex(request), hex(response)] {
            let msg = decode(&datagram).expect("decodes");
            assert!(msg.flags.auth);
            assert_eq!(msg.usm.auth_params.len(), usm::auth_params_len(protocol));
            assert!(
                msg.usm.auth_params.iter().any(|&octet| octet != 0),
                "a digest is present"
            );
            assert_eq!(
                recomputed(&datagram, protocol, password),
                msg.usm.auth_params
            );
        }
    }

    /// What does not decode, and why.
    #[rstest]
    #[case::truncated(edited(|d| { d.pop(); }), BerError::Truncated)]
    #[case::trailing_octet(edited(|d| d.push(0)), BerError::BadLength)]
    #[case::version_1(edited(|d| d[4] = 1), BerError::UnsupportedVersion(1))]
    #[case::two_flag_octets(two_flag_octets(), BerError::BadLength)]
    #[case::msg_max_size_483(msg_max_size(483), BerError::Overflow)]
    #[case::negative_boots(negative_boots(), BerError::Overflow)]
    #[case::privacy_flag_over_plaintext(edited(|d| d[20] |= 0x02), BerError::BadTag(0x30))]
    #[case::trap_pdu(edited(|d| { let at = d.len() - 16; d[at] = 0xa4; }), BerError::BadTag(0xa4))]
    #[case::empty(Vec::new(), BerError::Truncated)]
    fn a_malformed_message_is_refused_with_its_error(
        #[case] datagram: Vec<u8>,
        #[case] expected: BerError,
    ) {
        assert_eq!(decode(&datagram), Err(expected));
    }

    /// The discovery request with one edit; the edit is checked to have
    /// landed where it was meant to by the guard in each builder.
    fn edited(edit: fn(&mut Vec<u8>)) -> Vec<u8> {
        let mut datagram = hex(NET_SNMP_V3_SHA256_DISCOVERY);
        assert!(decode(&datagram).is_ok(), "the unedited request decodes");
        assert_eq!(
            (datagram[4], datagram[20], datagram[datagram.len() - 16]),
            (3, 0x04, 0xa0)
        );
        edit(&mut datagram);
        datagram
    }

    /// A request from a client whose msgMaxSize is `size`.
    fn msg_max_size(size: i64) -> Vec<u8> {
        let mut datagram = Client::new("watcher", None).get(1, &["1.3.6.1.2.1.1.5.0"]);
        // msgMaxSize is the second INTEGER of msgGlobalData: 02 03 00 ff e3.
        let at = datagram
            .windows(5)
            .position(|w| w == [0x02, 0x03, 0x00, 0xff, 0xe3])
            .expect("65507 is there");
        datagram[at + 2..at + 5].copy_from_slice(&size.to_be_bytes()[5..]);
        datagram
    }

    /// The discovery request with msgFlags two octets long.
    fn two_flag_octets() -> Vec<u8> {
        let request = hex(NET_SNMP_V3_SHA256_DISCOVERY);
        let flags = request
            .windows(3)
            .position(|w| w == [0x04, 0x01, 0x04])
            .expect("msgFlags");
        let mut datagram = request[..flags].to_vec();
        datagram.extend_from_slice(&[0x04, 0x02, 0x04, 0x00]);
        datagram.extend_from_slice(&request[flags + 3..]);
        // Two more octets in the outer SEQUENCE and in msgGlobalData.
        datagram[1] += 1;
        datagram[6] += 1;
        datagram
    }

    /// A request carrying engine boots of -1.
    fn negative_boots() -> Vec<u8> {
        let mut client = Client::new("watcher", None);
        client.engine_id = hex(CAPTURE_ENGINE_ID);
        client.boots = 0x7f;
        let mut datagram = client.get(1, &["1.3.6.1.2.1.1.5.0"]);
        let at = datagram
            .windows(3)
            .position(|w| w == [0x02, 0x01, 0x7f])
            .expect("boots");
        datagram[at + 2] = 0xff;
        datagram
    }

    /// [`response_len`] is the length [`write_response`] writes, across the
    /// lengths where the outer SEQUENCE's length takes one, two and three
    /// octets, authenticated by each protocol and not.
    #[test]
    fn response_len_is_the_length_written() {
        let request = hex(NET_SNMP_V3_SHA256_GET);
        let msg = decode(&request).expect("decodes");
        let engine = hex(CAPTURE_ENGINE_ID);
        let users: Vec<LocalizedUser> = capture_users()
            .iter()
            .map(|user| LocalizedUser::new(user, &engine))
            .collect();
        let mut checked = 0;
        for time in [0, 127, 128, 32_768] {
            let snapshot = Snapshot {
                id: &engine,
                boots: 1,
                time,
            };
            for user in users.iter().map(Some).chain([None]) {
                for pdu_len in (0..400).step_by(7) {
                    let mut out = vec![0xee; 3];
                    write_response(&mut out, &msg, &snapshot, user, |out| {
                        out.resize(out.len() + pdu_len, 0x55)
                    });
                    assert_eq!(
                        out.len() - 3,
                        response_len(&msg, &snapshot, user, pdu_len),
                        "{time} {pdu_len}"
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 4 * 8 * 58);
    }

    /// An authenticated response carries the request's msgID and user, the
    /// engine's ID, boots and time, the authentication flag and not the
    /// reportable one, the engine as its context and an empty context name,
    /// and the HMAC of itself with the parameter zeroed, written into a
    /// buffer that already holds other bytes.
    #[test]
    fn an_authenticated_response_carries_its_own_hmac() {
        let request = hex(NET_SNMP_V3_SHA512_GET);
        let msg = decode(&request).expect("decodes");
        let engine = hex(CAPTURE_ENGINE_ID);
        let user = LocalizedUser::new(&capture_users()[5], &engine);
        let snapshot = Snapshot {
            id: &engine,
            boots: 1,
            time: 300,
        };
        let mut out = vec![0xee; 5];
        // Long enough for the outer length to take two octets.
        write_response(&mut out, &msg, &snapshot, Some(&user), |out| {
            ber::write_tlv(out, Tag::Response, |out| {
                ber::write_i64(out, Tag::Integer, 9);
                ber::write_i64(out, Tag::Integer, 0);
                ber::write_i64(out, Tag::Integer, 0);
                ber::write_tlv(out, Tag::Sequence, |out| out.resize(out.len() + 300, 0x05));
            });
        });
        assert_eq!(&out[..5], &[0xee; 5]);
        let response = &out[5..];
        assert_eq!(response[1], 0x82, "a two-octet outer length");
        let reply = decode(response).expect("decodes");
        assert_eq!(reply.msg_id, msg.msg_id);
        assert_eq!(
            reply.flags,
            Flags {
                auth: true,
                privacy: false,
                reportable: false
            }
        );
        assert_eq!(
            (
                reply.usm.engine_id,
                reply.usm.boots,
                reply.usm.time,
                reply.usm.user
            ),
            (&engine[..], 1, 300, &b"sha512user"[..])
        );
        let scoped = reply.scoped.expect("plaintext");
        assert_eq!(
            (scoped.context_engine_id, scoped.context_name),
            (&engine[..], &b""[..])
        );
        assert_eq!(scoped.pdu.request_id, 9);
        assert_eq!(
            recomputed(response, AuthProtocol::Sha512, "lab-sha512-pass"),
            reply.usm.auth_params
        );
    }
}
