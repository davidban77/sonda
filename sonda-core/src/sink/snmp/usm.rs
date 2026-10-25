//! The User-based Security Model (RFC 3414) of an agent answering SNMPv3:
//! the agent as the authoritative engine, its users' localized keys, and
//! the checks a request passes before it is answered.
//!
//! The agent's engine boots once per bind (`engineBoots` is 1 on every run;
//! it is not persisted) and its clock counts whole seconds from the bind. A
//! client learns both, with the engine ID, from the report it gets to a
//! request carrying an empty engine ID, and must stay within 150 seconds of
//! the clock after that.

use hmac::digest::Digest;
use hmac::{Hmac, Mac};
use tokio::time::Instant;

use std::net::SocketAddr;

use super::agent::Counter;
use super::{derived_engine_id, AgentSettings, AuthProtocol, UsmUser};
use crate::encoder::snmp::v3::V3Message;

/// The largest `engineBoots` and `engineTime` (RFC 3414): 2³¹ − 1. An
/// engine whose boots reach it accepts no authenticated request.
pub(crate) const MAX_BOOTS_OR_TIME: u32 = i32::MAX as u32;

/// The time window: how far a request's `engineTime` may be from the
/// engine's, in seconds (RFC 3414 §2.2.3).
pub(crate) const TIME_WINDOW: u32 = 150;

/// The longest authentication parameter, HMAC-SHA-512's 48 octets.
pub(crate) const MAX_AUTH_PARAMS: usize = 48;

/// The octets a password is stretched to before it is hashed into a key
/// (RFC 3414 A.2).
const STRETCHED_PASSWORD: usize = 1_048_576;

/// The `engineBoots` of every agent: it boots once per bind.
const BOOTS: u32 = 1;

/// The agent as the SNMPv3 authoritative engine: its ID, its boots and the
/// instant its clock counts from.
#[derive(Debug)]
pub(crate) struct Engine {
    id: Vec<u8>,
    boots: u32,
    bound_at: Instant,
}

/// What an [`Engine`] reports at one instant: its ID, boots and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Snapshot<'a> {
    pub(crate) id: &'a [u8],
    pub(crate) boots: u32,
    pub(crate) time: u32,
}

impl Engine {
    /// The engine `id`, booted once at `bound_at`.
    pub(crate) fn new(id: Vec<u8>, bound_at: Instant) -> Self {
        Self {
            id,
            boots: BOOTS,
            bound_at,
        }
    }

    /// The engine ID.
    pub(crate) fn id(&self) -> &[u8] {
        &self.id
    }

    /// Whole seconds since the engine booted, at most 2³¹ − 1.
    pub(crate) fn time(&self) -> u32 {
        let seconds = self.bound_at.elapsed().as_secs();
        u32::try_from(seconds)
            .unwrap_or(u32::MAX)
            .min(MAX_BOOTS_OR_TIME)
    }

    /// Whether a request's `boots` and `time` are within the engine's time
    /// window (RFC 3414 §3.2 step 7b): the engine's boots are below
    /// 2³¹ − 1, `boots` equals them, and `time` is at most 150 seconds from
    /// the engine's.
    pub(crate) fn in_window(&self, boots: u32, time: u32) -> bool {
        self.boots < MAX_BOOTS_OR_TIME
            && boots == self.boots
            && time.abs_diff(self.time()) <= TIME_WINDOW
    }

    /// The engine's ID, boots and time now.
    pub(crate) fn snapshot(&self) -> Snapshot<'_> {
        Snapshot {
            id: &self.id,
            boots: self.boots,
            time: self.time(),
        }
    }
}

/// A user with its key localized to the engine.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LocalizedUser {
    name: Vec<u8>,
    /// The protocol and the localized key (Kul), when the user's requests
    /// are authenticated.
    auth: Option<(AuthProtocol, Vec<u8>)>,
}

/// Prints the name, the protocol, and `"***"` in place of the key.
impl std::fmt::Debug for LocalizedUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalizedUser")
            .field("name", &String::from_utf8_lossy(&self.name))
            .field("auth", &self.auth.as_ref().map(|(protocol, _)| protocol))
            .field("key", &"***")
            .finish()
    }
}

impl LocalizedUser {
    /// `user` with its password localized to `engine_id`.
    pub(crate) fn new(user: &UsmUser, engine_id: &[u8]) -> Self {
        Self {
            name: user.name.as_bytes().to_vec(),
            auth: user.auth.as_ref().map(|auth| {
                (
                    auth.protocol,
                    localize(auth.protocol, auth.password.as_bytes(), engine_id),
                )
            }),
        }
    }

    /// The user name.
    #[cfg(test)]
    pub(crate) fn name(&self) -> &[u8] {
        &self.name
    }

    /// The protocol and the localized key, when the user authenticates.
    pub(crate) fn auth(&self) -> Option<(AuthProtocol, &[u8])> {
        self.auth
            .as_ref()
            .map(|(protocol, key)| (*protocol, key.as_slice()))
    }
}

/// The engine and the users of an agent answering SNMPv3, each user's key
/// localized once, when the agent binds.
#[derive(Debug)]
pub(crate) struct Usm {
    engine: Engine,
    users: Vec<LocalizedUser>,
}

impl Usm {
    /// The engine of `settings.engine_id`, or of the ID derived from
    /// `local_addr` without one, booted at `bound_at`, and `settings.users`
    /// with their keys localized to it.
    pub(crate) fn new(settings: &AgentSettings, local_addr: SocketAddr, bound_at: Instant) -> Self {
        let id = settings
            .engine_id
            .clone()
            .unwrap_or_else(|| derived_engine_id(local_addr));
        let users = settings
            .users
            .iter()
            .map(|user| LocalizedUser::new(user, &id))
            .collect();
        Self {
            engine: Engine::new(id, bound_at),
            users,
        }
    }

    /// The engine.
    pub(crate) fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The users, ordered by name.
    pub(crate) fn users(&self) -> &[LocalizedUser] {
        &self.users
    }
}

/// The user of `users` named `name`.
pub(crate) fn user<'u>(users: &'u [LocalizedUser], name: &[u8]) -> Option<&'u LocalizedUser> {
    users.iter().find(|user| user.name == name)
}

/// The key `password` localizes to for `engine_id` under `protocol` (RFC
/// 3414 A.2, and RFC 7860 for the SHA-2 protocols): the password repeated
/// to 1 048 576 octets and hashed into Ku, then Kul = H(Ku ‖ engine ID ‖
/// Ku). This is the one definition of localization.
pub(crate) fn localize(protocol: AuthProtocol, password: &[u8], engine_id: &[u8]) -> Vec<u8> {
    match protocol {
        AuthProtocol::Md5 => localize_with::<md5::Md5>(password, engine_id),
        AuthProtocol::Sha1 => localize_with::<sha1::Sha1>(password, engine_id),
        AuthProtocol::Sha224 => localize_with::<sha2::Sha224>(password, engine_id),
        AuthProtocol::Sha256 => localize_with::<sha2::Sha256>(password, engine_id),
        AuthProtocol::Sha384 => localize_with::<sha2::Sha384>(password, engine_id),
        AuthProtocol::Sha512 => localize_with::<sha2::Sha512>(password, engine_id),
    }
}

/// Ku, the key `password` makes before it is localized (RFC 3414 A.2).
fn password_to_key<D: Digest>(password: &[u8]) -> Vec<u8> {
    let mut hash = D::new();
    let mut block = [0u8; 64];
    let mut stretched = password.iter().cycle();
    for _ in 0..STRETCHED_PASSWORD / block.len() {
        for (octet, from) in block.iter_mut().zip(stretched.by_ref()) {
            *octet = *from;
        }
        hash.update(block);
    }
    hash.finalize().to_vec()
}

fn localize_with<D: Digest>(password: &[u8], engine_id: &[u8]) -> Vec<u8> {
    let key = password_to_key::<D>(password);
    let mut hash = D::new();
    hash.update(&key);
    hash.update(engine_id);
    hash.update(&key);
    hash.finalize().to_vec()
}

/// The authentication parameter's length under `protocol`: the HMAC
/// truncated to 12 octets for MD5 and SHA-1 (RFC 3414), 16, 24, 32 and 48
/// for SHA-224, SHA-256, SHA-384 and SHA-512 (RFC 7860 §4.2).
pub(crate) fn auth_params_len(protocol: AuthProtocol) -> usize {
    match protocol {
        AuthProtocol::Md5 | AuthProtocol::Sha1 => 12,
        AuthProtocol::Sha224 => 16,
        AuthProtocol::Sha256 => 24,
        AuthProtocol::Sha384 => 32,
        AuthProtocol::Sha512 => 48,
    }
}

/// Write into `out` the first `out.len()` octets of the HMAC of `message`
/// under `protocol` with `key`. `out` is as long as
/// [`auth_params_len`] of the protocol.
pub(crate) fn hmac(protocol: AuthProtocol, key: &[u8], message: &[u8], out: &mut [u8]) {
    hmac_of(protocol, key, &[message], out);
}

/// [`hmac`] of the concatenation of `parts`.
fn hmac_of(protocol: AuthProtocol, key: &[u8], parts: &[&[u8]], out: &mut [u8]) {
    match protocol {
        AuthProtocol::Md5 => hmac_with::<Hmac<md5::Md5>>(key, parts, out),
        AuthProtocol::Sha1 => hmac_with::<Hmac<sha1::Sha1>>(key, parts, out),
        AuthProtocol::Sha224 => hmac_with::<Hmac<sha2::Sha224>>(key, parts, out),
        AuthProtocol::Sha256 => hmac_with::<Hmac<sha2::Sha256>>(key, parts, out),
        AuthProtocol::Sha384 => hmac_with::<Hmac<sha2::Sha384>>(key, parts, out),
        AuthProtocol::Sha512 => hmac_with::<Hmac<sha2::Sha512>>(key, parts, out),
    }
}

fn hmac_with<M: Mac + hmac::digest::KeyInit>(key: &[u8], parts: &[&[u8]], out: &mut [u8]) {
    let mut mac =
        <M as hmac::digest::KeyInit>::new_from_slice(key).expect("HMAC takes a key of any length");
    for part in parts {
        mac.update(part);
    }
    let tag = mac.finalize().into_bytes();
    let len = out.len().min(tag.len());
    out[..len].copy_from_slice(&tag[..len]);
}

/// A usmStats counter (RFC 3414 §5), each reported in a `Report` PDU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsmStat {
    /// `usmStatsUnknownEngineIDs`: the request's engine ID is not the
    /// agent's, or empty (discovery).
    UnknownEngineIds,
    /// `usmStatsUnknownUserNames`: no user has the request's name.
    UnknownUserNames,
    /// `usmStatsUnsupportedSecLevels`: the request asks for privacy, or for
    /// a security level other than its user's.
    UnsupportedSecLevels,
    /// `usmStatsWrongDigests`: the request's HMAC is not the user's.
    WrongDigests,
    /// `usmStatsNotInTimeWindows`: the request's boots or time are outside
    /// the time window.
    NotInTimeWindows,
}

impl UsmStat {
    /// The OBJECT IDENTIFIER body of the counter's instance,
    /// `1.3.6.1.6.3.15.1.1.N.0`.
    pub(crate) fn oid_body(self) -> &'static [u8] {
        match self {
            UsmStat::UnsupportedSecLevels => &[0x2b, 6, 1, 6, 3, 15, 1, 1, 1, 0],
            UsmStat::NotInTimeWindows => &[0x2b, 6, 1, 6, 3, 15, 1, 1, 2, 0],
            UsmStat::UnknownUserNames => &[0x2b, 6, 1, 6, 3, 15, 1, 1, 3, 0],
            UsmStat::UnknownEngineIds => &[0x2b, 6, 1, 6, 3, 15, 1, 1, 4, 0],
            UsmStat::WrongDigests => &[0x2b, 6, 1, 6, 3, 15, 1, 1, 5, 0],
        }
    }

    /// The agent's counter of the reports of this stat.
    pub(crate) fn counter(self) -> Counter {
        match self {
            UsmStat::UnknownEngineIds => Counter::UsmUnknownEngineIds,
            UsmStat::UnknownUserNames => Counter::UsmUnknownUserNames,
            UsmStat::UnsupportedSecLevels => Counter::UsmUnsupportedSecLevels,
            UsmStat::WrongDigests => Counter::UsmWrongDigests,
            UsmStat::NotInTimeWindows => Counter::UsmNotInTimeWindows,
        }
    }
}

/// What [`verify`] decides for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsmVerdict<'u> {
    /// Answer the scoped PDU for `user`, authenticated with its key when
    /// `authenticated`.
    Answer {
        user: &'u LocalizedUser,
        authenticated: bool,
    },
    /// Answer with a `Report` of `stat`, authenticated with the request's
    /// user's key when `authenticated`.
    Report { stat: UsmStat, authenticated: bool },
}

/// Whether the SNMPv3 request `msg`, decoded from `raw`, is answered, or
/// reported, by an agent with `engine` and `users` (RFC 3414 §3.2). Pure.
///
/// The checks run in this order, the first that fails deciding:
///
/// 1. The engine ID is empty or another engine's: `UnknownEngineIds`,
///    unauthenticated. This is discovery.
/// 2. The privacy flag is set: `UnsupportedSecLevels`.
/// 3. No user has the request's name: `UnknownUserNames`.
/// 4. The authentication flag is set for a user without `auth`, or clear for
///    a user with it: `UnsupportedSecLevels`. A user's level is its minimum.
/// 5. The HMAC over the whole message, its authentication parameter zeroed,
///    is not the one the user's key makes (compared in constant time), or
///    the parameter is not the protocol's length: `WrongDigests`,
///    unauthenticated.
/// 6. The engine's boots are 2³¹ − 1, the request's boots are not the
///    engine's, or its time is more than 150 seconds from the engine's:
///    `NotInTimeWindows`, the one report that is authenticated.
///
/// Otherwise the request is answered for its user, authenticated when its
/// authentication flag is set. Every report before step 6 is
/// unauthenticated.
pub(crate) fn verify<'u>(
    msg: &V3Message<'_>,
    raw: &[u8],
    engine: &Engine,
    users: &'u [LocalizedUser],
) -> UsmVerdict<'u> {
    let report = |stat, authenticated| UsmVerdict::Report {
        stat,
        authenticated,
    };
    let usm = &msg.usm;
    if usm.engine_id.is_empty() || usm.engine_id != engine.id() {
        return report(UsmStat::UnknownEngineIds, false);
    }
    if msg.flags.privacy {
        return report(UsmStat::UnsupportedSecLevels, false);
    }
    let Some(user) = user(users, usm.user) else {
        return report(UsmStat::UnknownUserNames, false);
    };
    let (protocol, key) = match (user.auth(), msg.flags.auth) {
        (Some(auth), true) => auth,
        (None, false) => {
            return UsmVerdict::Answer {
                user,
                authenticated: false,
            }
        }
        _ => return report(UsmStat::UnsupportedSecLevels, false),
    };
    if !authentic(protocol, key, msg, raw) {
        return report(UsmStat::WrongDigests, false);
    }
    if !engine.in_window(usm.boots, usm.time) {
        return report(UsmStat::NotInTimeWindows, true);
    }
    UsmVerdict::Answer {
        user,
        authenticated: true,
    }
}

/// Whether `msg`'s authentication parameter is the HMAC of `raw` with the
/// parameter zeroed, under `protocol` with `key`, compared in constant
/// time. `false` when the parameter is not the protocol's length.
fn authentic(protocol: AuthProtocol, key: &[u8], msg: &V3Message<'_>, raw: &[u8]) -> bool {
    const ZEROS: [u8; MAX_AUTH_PARAMS] = [0; MAX_AUTH_PARAMS];
    let len = auth_params_len(protocol);
    let range = msg.auth_range.clone();
    let (Some(before), Some(after)) = (raw.get(..range.start), raw.get(range.end..)) else {
        return false;
    };
    if msg.usm.auth_params.len() != len || range.len() != len {
        return false;
    }
    let mut digest = [0u8; MAX_AUTH_PARAMS];
    hmac_of(
        protocol,
        key,
        &[before, &ZEROS[..len], after],
        &mut digest[..len],
    );
    crate::util::constant_time_eq(&digest[..len], msg.usm.auth_params)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::encoder::snmp::oid::Oid;
    use crate::encoder::snmp::v3;
    use crate::sink::snmp::tests::hex;
    use crate::sink::snmp::UsmAuth;

    /// RFC 3414 A.3's engine ID, `00 00 00 00 00 00 00 00 00 00 00 02`.
    const RFC_ENGINE_ID: &str = "000000000000000000000002";

    /// RFC 3414 A.3.1: Ku and Kul of `maplesyrup` under MD5.
    #[test]
    fn md5_localizes_maplesyrup_as_rfc_3414_a_3_1() {
        assert_eq!(
            password_to_key::<md5::Md5>(b"maplesyrup"),
            hex("9faf3283884e92834ebc9847d8edd963")
        );
        assert_eq!(
            localize(AuthProtocol::Md5, b"maplesyrup", &hex(RFC_ENGINE_ID)),
            hex("526f5eed9fcce26f8964c2930787d82b")
        );
    }

    /// RFC 3414 A.3.2: Ku and Kul of `maplesyrup` under SHA-1.
    #[test]
    fn sha1_localizes_maplesyrup_as_rfc_3414_a_3_2() {
        assert_eq!(
            password_to_key::<sha1::Sha1>(b"maplesyrup"),
            hex("9fb5cc0381497b3793528939ff788d5d79145211")
        );
        assert_eq!(
            localize(AuthProtocol::Sha1, b"maplesyrup", &hex(RFC_ENGINE_ID)),
            hex("6695febc9288e36282235fc7151f128497b38f3f")
        );
    }

    /// Each protocol's key is its hash's length, and each authentication
    /// parameter the truncation RFC 3414 and RFC 7860 give it.
    #[rustfmt::skip]
    #[rstest]
    #[case::md5(   AuthProtocol::Md5,    16, 12)]
    #[case::sha1(  AuthProtocol::Sha1,   20, 12)]
    #[case::sha224(AuthProtocol::Sha224, 28, 16)]
    #[case::sha256(AuthProtocol::Sha256, 32, 24)]
    #[case::sha384(AuthProtocol::Sha384, 48, 32)]
    #[case::sha512(AuthProtocol::Sha512, 64, 48)]
    fn each_protocol_has_its_key_and_truncation(
        #[case] protocol: AuthProtocol,
        #[case] key: usize,
        #[case] truncated: usize,
    ) {
        assert_eq!(localize(protocol, b"maplesyrup", &hex(RFC_ENGINE_ID)).len(), key);
        assert_eq!(auth_params_len(protocol), truncated);
        assert!(truncated <= MAX_AUTH_PARAMS);
    }

    /// Each usmStats OID body is the OID it names.
    #[rustfmt::skip]
    #[rstest]
    #[case::unsupported_sec_levels(UsmStat::UnsupportedSecLevels, "1.3.6.1.6.3.15.1.1.1.0")]
    #[case::not_in_time_windows(   UsmStat::NotInTimeWindows,     "1.3.6.1.6.3.15.1.1.2.0")]
    #[case::unknown_user_names(    UsmStat::UnknownUserNames,     "1.3.6.1.6.3.15.1.1.3.0")]
    #[case::unknown_engine_ids(    UsmStat::UnknownEngineIds,     "1.3.6.1.6.3.15.1.1.4.0")]
    #[case::wrong_digests(         UsmStat::WrongDigests,         "1.3.6.1.6.3.15.1.1.5.0")]
    fn each_stat_knows_its_oid(#[case] stat: UsmStat, #[case] oid: &str) {
        let oid = Oid::parse(oid).expect("an OID");
        let mut body = Vec::new();
        oid.write_ber_body(&mut body);
        assert_eq!(stat.oid_body(), body.as_slice());
    }

    // ---- verify -------------------------------------------------------------

    /// The engine every `verify` case runs against.
    const ENGINE_ID: &str = "80001f8804736f6e64613a7465737430";

    fn engine() -> Engine {
        Engine::new(hex(ENGINE_ID), Instant::now())
    }

    fn users() -> Vec<LocalizedUser> {
        [
            UsmUser {
                name: "telegraf".to_string(),
                auth: Some(UsmAuth {
                    protocol: AuthProtocol::Sha256,
                    password: "lab-only-password".to_string(),
                }),
            },
            UsmUser {
                name: "watcher".to_string(),
                auth: None,
            },
        ]
        .iter()
        .map(|user| LocalizedUser::new(user, &hex(ENGINE_ID)))
        .collect()
    }

    /// One defect a request may carry.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Defect {
        None,
        EmptyEngine,
        ForeignEngine,
        UnknownUser,
        AuthWithoutKey,
        KeyWithoutAuth,
        Privacy,
        WrongDigest,
        ShortDigest,
        BootsOffByOne,
        Time151Behind,
        Time151Ahead,
        Time150Behind,
        BootsAtMax,
    }

    /// A GET of sysUpTime.0 from `name`, at the engine's boots and time
    /// unless `defect` says otherwise, authenticated with `name`'s key when
    /// its flags say so.
    fn request(engine: &Engine, users: &[LocalizedUser], name: &str, defect: Defect) -> Vec<u8> {
        let time = engine.time();
        let mut client = v3::tests::Client {
            engine_id: hex(ENGINE_ID),
            boots: 1,
            time,
            ..v3::tests::Client::new(
                name,
                user(users, name.as_bytes()).and_then(|u| u.auth.clone()),
            )
        };
        match defect {
            Defect::None | Defect::WrongDigest | Defect::ShortDigest => {}
            Defect::EmptyEngine => client.engine_id.clear(),
            Defect::ForeignEngine => client.engine_id = hex("80001f8804666f726569676e"),
            Defect::UnknownUser => client.user = b"stranger".to_vec(),
            Defect::AuthWithoutKey => {
                client.key = Some((
                    AuthProtocol::Sha256,
                    localize(AuthProtocol::Sha256, b"lab-only-password", &hex(ENGINE_ID)),
                ))
            }
            Defect::KeyWithoutAuth => client.key = None,
            Defect::Privacy => client.privacy = true,
            Defect::BootsOffByOne => client.boots = 2,
            Defect::Time151Behind => client.time = time.wrapping_sub(151),
            Defect::Time151Ahead => client.time = time + 151,
            Defect::Time150Behind => client.time = time.wrapping_sub(150),
            Defect::BootsAtMax => client.boots = MAX_BOOTS_OR_TIME,
        }
        let mut datagram = client.get(0x1234, &["1.3.6.1.2.1.1.3.0"]);
        match defect {
            Defect::WrongDigest => {
                let msg = v3::decode(&datagram).expect("decodes");
                let at = msg.auth_range.start;
                datagram[at] ^= 0x01;
            }
            Defect::ShortDigest => {
                datagram = client.get_with_auth_len(0x1234, &["1.3.6.1.2.1.1.3.0"], 12);
            }
            _ => {}
        }
        datagram
    }

    /// `verify`'s verdict for `defect` in a request from `name`.
    fn verdict(engine: &Engine, users: &[LocalizedUser], name: &str, defect: Defect) -> String {
        let datagram = request(engine, users, name, defect);
        let msg = v3::decode(&datagram).expect("the request decodes");
        match verify(&msg, &datagram, engine, users) {
            UsmVerdict::Answer {
                user,
                authenticated,
            } => format!(
                "answer {} {}",
                String::from_utf8_lossy(user.name()),
                if authenticated {
                    "authenticated"
                } else {
                    "unauthenticated"
                }
            ),
            UsmVerdict::Report {
                stat,
                authenticated,
            } => format!(
                "report {stat:?} {}",
                if authenticated {
                    "authenticated"
                } else {
                    "unauthenticated"
                }
            ),
        }
    }

    /// Each refusal of RFC 3414 §3.2, in `verify`'s order. Guard: the same
    /// request without its defect is answered, so the defect is what the
    /// verdict sees.
    #[rustfmt::skip]
    #[rstest]
    #[case::empty_engine_is_discovery(Defect::EmptyEngine,    "telegraf", "report UnknownEngineIds unauthenticated")]
    #[case::foreign_engine(           Defect::ForeignEngine,  "telegraf", "report UnknownEngineIds unauthenticated")]
    #[case::unknown_user(             Defect::UnknownUser,    "telegraf", "report UnknownUserNames unauthenticated")]
    #[case::auth_flag_without_a_key(  Defect::AuthWithoutKey, "watcher",  "report UnsupportedSecLevels unauthenticated")]
    #[case::key_without_the_auth_flag(Defect::KeyWithoutAuth, "telegraf", "report UnsupportedSecLevels unauthenticated")]
    #[case::privacy_flag(             Defect::Privacy,        "telegraf", "report UnsupportedSecLevels unauthenticated")]
    #[case::wrong_digest(             Defect::WrongDigest,    "telegraf", "report WrongDigests unauthenticated")]
    #[case::digest_of_the_wrong_length(Defect::ShortDigest,   "telegraf", "report WrongDigests unauthenticated")]
    #[case::boots_off_by_one(         Defect::BootsOffByOne,  "telegraf", "report NotInTimeWindows authenticated")]
    #[case::time_151_seconds_behind(  Defect::Time151Behind,  "telegraf", "report NotInTimeWindows authenticated")]
    #[case::time_151_seconds_ahead(   Defect::Time151Ahead,   "telegraf", "report NotInTimeWindows authenticated")]
    #[case::boots_at_the_maximum(     Defect::BootsAtMax,     "telegraf", "report NotInTimeWindows authenticated")]
    #[case::time_150_seconds_behind(  Defect::Time150Behind,  "telegraf", "answer telegraf authenticated")]
    #[case::auth_no_priv(             Defect::None,           "telegraf", "answer telegraf authenticated")]
    #[case::no_auth_no_priv(          Defect::None,           "watcher",  "answer watcher unauthenticated")]
    #[tokio::test(start_paused = true)]
    async fn verify_answers_or_reports_in_rfc_3414_order(
        #[case] defect: Defect,
        #[case] name: &str,
        #[case] expected: &str,
    ) {
        let engine = engine();
        // Far enough from boot that a time 151 seconds behind is positive.
        tokio::time::advance(std::time::Duration::from_secs(1000)).await;
        let users = users();
        let clean = if name == "watcher" { "answer watcher unauthenticated" } else { "answer telegraf authenticated" };
        assert_eq!(verdict(&engine, &users, name, Defect::None), clean, "the request without its defect");
        assert_eq!(verdict(&engine, &users, name, defect), expected);
    }

    /// An engine whose boots reached 2³¹ − 1 refuses a request carrying the
    /// same boots: the latch of RFC 3414 §2.2.2.
    #[tokio::test(start_paused = true)]
    async fn an_engine_at_the_last_boot_accepts_no_authenticated_request() {
        let mut latched = engine();
        let users = users();
        let client_at = |boots: u32, engine: &Engine| {
            let mut client = v3::tests::Client {
                engine_id: hex(ENGINE_ID),
                boots,
                time: engine.time(),
                ..v3::tests::Client::new(
                    "telegraf",
                    user(&users, b"telegraf").and_then(|u| u.auth.clone()),
                )
            };
            let datagram = client.get(7, &["1.3.6.1.2.1.1.3.0"]);
            let msg = v3::decode(&datagram).expect("decodes");
            matches!(
                verify(&msg, &datagram, engine, &users),
                UsmVerdict::Answer { .. }
            )
        };
        assert!(client_at(1, &latched), "answered at boots 1");
        latched.boots = MAX_BOOTS_OR_TIME;
        assert!(!client_at(MAX_BOOTS_OR_TIME, &latched));
    }

    /// The engine's time counts whole seconds from the bind.
    #[tokio::test(start_paused = true)]
    async fn engine_time_counts_whole_seconds_since_the_bind() {
        let engine = engine();
        assert_eq!(engine.time(), 0);
        tokio::time::advance(std::time::Duration::from_millis(1999)).await;
        assert_eq!(engine.time(), 1);
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        assert_eq!(engine.time(), 2);
        assert_eq!(
            engine.snapshot(),
            Snapshot {
                id: engine.id(),
                boots: 1,
                time: 2
            }
        );
    }

    /// Debug prints no key.
    #[test]
    fn debug_prints_no_key() {
        let users = users();
        let debug = format!("{:?}", users[0]);
        assert!(
            debug.contains("telegraf") && debug.contains("Sha256"),
            "{debug}"
        );
        let key = users[0].auth().map(|(_, key)| key).expect("a key");
        assert!(!debug.contains(&format!("{:?}", key)), "{debug}");
    }
}
