//! The User-based Security Model (RFC 3414) of an agent answering SNMPv3:
//! the agent as the authoritative engine, its users' localized keys, the
//! checks a request passes before it is answered, and the ciphers of its
//! privacy protocols.
//!
//! The agent's engine boots once per bind (`engineBoots` is 1 on every run;
//! it is not persisted) and its clock counts whole seconds from the bind. A
//! client learns both, with the engine ID, from the report it gets to a
//! request carrying an empty engine ID, and must stay within 150 seconds of
//! the clock after that.
//!
//! A user with privacy has a second key, localized from its privacy
//! password with its authentication hash and extended when the cipher needs
//! more octets than the hash gives. A request at `authPriv` carries its
//! scoped PDU encrypted, with the salt it was encrypted with in its privacy
//! parameter; each response is encrypted with a fresh salt from the engine's
//! counters.

use std::hash::BuildHasher;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use cbc::cipher::generic_array::GenericArray;
use cbc::cipher::{
    consts::U16, AsyncStreamCipher, BlockCipher, BlockDecryptMut, BlockEncryptMut, KeyInit,
    KeyIvInit,
};
use hmac::digest::Digest;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::time::Instant;

use super::agent::Counter;
use super::{AgentSettings, AuthProtocol, PrivProtocol, UsmUser};
use crate::encoder::snmp::v3::{self, ScopedPdu, V3Message};

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

/// The first five octets of a default engine ID: net-snmp's enterprise
/// number 8072 with the high bit set, then format 5, administratively
/// assigned octets (RFC 3411).
const DEFAULT_ENGINE_ID_PREFIX: [u8; 5] = [0x80, 0x00, 0x1f, 0x88, 0x05];

/// The octets of the SHA-256 a default engine ID carries after its prefix.
const DEFAULT_ENGINE_ID_HASH: usize = 16;

/// The length of a privacy parameter: the salt, 8 octets for DES and AES.
pub(crate) const PRIV_PARAMS: usize = 8;

/// The block of DES-CBC, which the plaintext is padded to.
const DES_BLOCK: usize = 8;

/// The agent as the SNMPv3 authoritative engine: its ID, its boots, the
/// instant its clock counts from, and the counters its privacy salts come
/// from.
#[derive(Debug)]
pub(crate) struct Engine {
    id: Vec<u8>,
    boots: u32,
    bound_at: Instant,
    salts: SaltCounter,
}

/// The engine's privacy salts: a 32-bit counter for DES and a 64-bit one
/// for AES, each started at a random value and raised by one per encrypted
/// message, so no salt repeats within a boot until its counter wraps.
#[derive(Debug)]
pub(crate) struct SaltCounter {
    des: AtomicU32,
    aes: AtomicU64,
}

impl SaltCounter {
    /// Counters started at random values.
    pub(crate) fn new() -> Self {
        Self::starting_at(random() as u32, random())
    }

    /// Counters starting at `des` and `aes`.
    pub(crate) fn starting_at(des: u32, aes: u64) -> Self {
        Self {
            des: AtomicU32::new(des),
            aes: AtomicU64::new(aes),
        }
    }

    /// The next DES salt counter value.
    fn next_des(&self) -> u32 {
        self.des.fetch_add(1, Ordering::Relaxed)
    }

    /// The next AES salt.
    fn next_aes(&self) -> u64 {
        self.aes.fetch_add(1, Ordering::Relaxed)
    }
}

/// A random 64-bit value: a hash under a fresh [`RandomState`], whose keys
/// std seeds from the operating system's random source.
///
/// [`RandomState`]: std::collections::hash_map::RandomState
fn random() -> u64 {
    std::collections::hash_map::RandomState::new().hash_one(std::process::id())
}

/// What an [`Engine`] reports at one instant: its ID, boots and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Snapshot<'a> {
    pub(crate) id: &'a [u8],
    pub(crate) boots: u32,
    pub(crate) time: u32,
}

impl Engine {
    /// The engine `id`, booted once at `bound_at`, its salt counters started
    /// at random.
    pub(crate) fn new(id: Vec<u8>, bound_at: Instant) -> Self {
        Self {
            id,
            boots: BOOTS,
            bound_at,
            salts: SaltCounter::new(),
        }
    }

    /// The engine ID.
    pub(crate) fn id(&self) -> &[u8] {
        &self.id
    }

    /// The counters the engine's privacy salts come from.
    pub(crate) fn salts(&self) -> &SaltCounter {
        &self.salts
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

/// A user with its keys localized to the engine.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LocalizedUser {
    name: Vec<u8>,
    /// The protocol and the localized key (Kul), when the user's requests
    /// are authenticated.
    auth: Option<(AuthProtocol, Vec<u8>)>,
    /// The protocol and the privacy key ([`extend_key`]), when the user's
    /// requests are encrypted.
    privacy: Option<(PrivProtocol, Vec<u8>)>,
}

/// Prints the name, the protocols, and `"***"` in place of the keys.
impl std::fmt::Debug for LocalizedUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalizedUser")
            .field("name", &String::from_utf8_lossy(&self.name))
            .field("auth", &self.auth.as_ref().map(|(protocol, _)| protocol))
            .field(
                "privacy",
                &self.privacy.as_ref().map(|(protocol, _)| protocol),
            )
            .field("key", &"***")
            .finish()
    }
}

impl LocalizedUser {
    /// `user` with its passwords localized to `engine_id`: the privacy
    /// password with the authentication protocol's hash (RFC 3414 §2.6),
    /// then extended for the cipher. A privacy block without `auth` is
    /// ignored; validation refuses it.
    pub(crate) fn new(user: &UsmUser, engine_id: &[u8]) -> Self {
        let auth = user.auth.as_ref().map(|auth| {
            (
                auth.protocol,
                localize(auth.protocol, auth.password.as_bytes(), engine_id),
            )
        });
        let privacy = user
            .auth
            .as_ref()
            .zip(user.privacy.as_ref())
            .map(|(auth, privacy)| {
                let kul = localize(auth.protocol, privacy.password.as_bytes(), engine_id);
                (
                    privacy.protocol,
                    extend_key(privacy.protocol, auth.protocol, &kul, engine_id),
                )
            });
        Self {
            name: user.name.as_bytes().to_vec(),
            auth,
            privacy,
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

    /// The protocol and the privacy key, when the user encrypts.
    pub(crate) fn privacy(&self) -> Option<(PrivProtocol, &[u8])> {
        self.privacy
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
    /// The engine of `settings.engine_id`, or without one of
    /// [`default_engine_id`] for this host and `local_addr`, booted at
    /// `bound_at`, and `settings.users` with their keys localized to it.
    pub(crate) fn new(settings: &AgentSettings, local_addr: SocketAddr, bound_at: Instant) -> Self {
        let id = settings
            .engine_id
            .clone()
            .unwrap_or_else(|| default_engine_id(&hostname(), local_addr));
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

/// The engine ID of an agent bound to `bound` on the host `hostname`
/// without a configured one, 21 octets: [`DEFAULT_ENGINE_ID_PREFIX`], then
/// the first 16 octets of the SHA-256 of `sonda:<hostname>:<bound>`.
pub(crate) fn default_engine_id(hostname: &[u8], bound: SocketAddr) -> Vec<u8> {
    let mut hash = Sha256::new();
    hash.update(b"sonda:");
    hash.update(hostname);
    hash.update(format!(":{bound}").as_bytes());
    let mut id = DEFAULT_ENGINE_ID_PREFIX.to_vec();
    id.extend_from_slice(&hash.finalize()[..DEFAULT_ENGINE_ID_HASH]);
    id
}

/// This host's name, or nothing when it cannot be read.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn hostname() -> Vec<u8> {
    use std::os::unix::ffi::OsStringExt;
    nix::unistd::gethostname()
        .map(OsStringExt::into_vec)
        .unwrap_or_default()
}

/// This host's name from the environment, or nothing.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn hostname() -> Vec<u8> {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .map(String::into_bytes)
        .unwrap_or_default()
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

/// The digest of `data` under `protocol`'s hash.
fn digest(protocol: AuthProtocol, data: &[u8]) -> Vec<u8> {
    match protocol {
        AuthProtocol::Md5 => md5::Md5::digest(data).to_vec(),
        AuthProtocol::Sha1 => sha1::Sha1::digest(data).to_vec(),
        AuthProtocol::Sha224 => sha2::Sha224::digest(data).to_vec(),
        AuthProtocol::Sha256 => sha2::Sha256::digest(data).to_vec(),
        AuthProtocol::Sha384 => sha2::Sha384::digest(data).to_vec(),
        AuthProtocol::Sha512 => sha2::Sha512::digest(data).to_vec(),
    }
}

/// The octets of privacy key `protocol` takes: DES's 8-octet key and its
/// 8-octet pre-IV, then AES's 16, 24 and 32.
pub(crate) fn key_len(protocol: PrivProtocol) -> usize {
    match protocol {
        PrivProtocol::Des | PrivProtocol::Aes128 => 16,
        PrivProtocol::Aes192 | PrivProtocol::Aes192c => 24,
        PrivProtocol::Aes256 | PrivProtocol::Aes256c => 32,
    }
}

/// The privacy key of `protocol` from `kul`, the privacy password localized
/// to `engine_id` under `auth`: its first [`key_len`] octets, extended
/// while it is shorter. This is the one definition of the extension.
///
/// - `aes192` and `aes256`, the Blumenthal extension
///   (draft-blumenthal-aes-usm-04): append the hash of the key so far.
/// - `aes192c` and `aes256c`, the Reeder extension
///   (draft-reeder-snmpv3-usm-3desede-00): append the previous part
///   localized as if it were a password.
///
/// DES and AES-128 need 16 octets, which every authentication hash gives,
/// and one round of either extension is enough for 32 octets from MD5's 16.
pub(crate) fn extend_key(
    protocol: PrivProtocol,
    auth: AuthProtocol,
    kul: &[u8],
    engine_id: &[u8],
) -> Vec<u8> {
    let needed = key_len(protocol);
    let mut key = kul.to_vec();
    let mut previous = kul.to_vec();
    while key.len() < needed && !previous.is_empty() {
        previous = match protocol {
            PrivProtocol::Aes192 | PrivProtocol::Aes256 => digest(auth, &key),
            _ => localize(auth, &previous, engine_id),
        };
        key.extend_from_slice(&previous);
    }
    key.truncate(needed);
    key
}

/// Why a scoped PDU could not be decrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrivError {
    /// The user has no privacy key.
    NoKey,
    /// The privacy parameter is not 8 octets.
    BadSalt,
    /// The DES ciphertext is not a whole number of 8-octet blocks.
    BadLength,
    /// The plaintext is not a scoped PDU (under DES followed by at most 7
    /// octets of padding): the key, the salt or the protocol is not the
    /// sender's.
    Garbled,
}

/// The octets `len` octets of plaintext take once encrypted under
/// `protocol`: a multiple of 8 for DES, whose padding is up to 7 octets,
/// and `len` for AES.
pub(crate) fn padded_len(protocol: PrivProtocol, len: usize) -> usize {
    match protocol {
        PrivProtocol::Des => len.div_ceil(DES_BLOCK) * DES_BLOCK,
        _ => len,
    }
}

/// `N` octets copied from the start of `bytes`, zeros past its end.
fn array<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    for (to, from) in out.iter_mut().zip(bytes) {
        *to = *from;
    }
    out
}

/// The IV a message is encrypted with under `protocol` with `key`: the DES
/// pre-IV XOR the salt (RFC 3414 §8.1.1.1), or `boots ‖ time ‖ salt` for
/// AES (RFC 3826 §3.1.2.1).
fn iv(protocol: PrivProtocol, key: &[u8], boots: u32, time: u32, salt: &[u8; 8]) -> [u8; 16] {
    let mut iv = [0u8; 16];
    match protocol {
        PrivProtocol::Des => {
            let pre_iv = key.get(8..16).unwrap_or_default();
            for ((to, pre), salt) in iv.iter_mut().zip(pre_iv).zip(salt) {
                *to = pre ^ salt;
            }
        }
        _ => {
            iv[..4].copy_from_slice(&boots.to_be_bytes());
            iv[4..8].copy_from_slice(&time.to_be_bytes());
            iv[8..].copy_from_slice(salt);
        }
    }
    iv
}

/// Encrypt `data` in place under CBC with the block cipher `C`, whose key
/// and block are `key` and `iv`; a partial last block is left as it is.
fn cbc_encrypt<C>(
    key: &GenericArray<u8, C::KeySize>,
    iv: &GenericArray<u8, C::BlockSize>,
    data: &mut [u8],
) where
    C: BlockEncryptMut + BlockCipher + KeyInit,
{
    let mut cipher = cbc::Encryptor::<C>::new(key, iv);
    for block in data.chunks_exact_mut(C::block_size()) {
        cipher.encrypt_block_mut(GenericArray::from_mut_slice(block));
    }
}

/// Decrypt `data` in place under CBC with the block cipher `C`; a partial
/// last block is left as it is.
fn cbc_decrypt<C>(
    key: &GenericArray<u8, C::KeySize>,
    iv: &GenericArray<u8, C::BlockSize>,
    data: &mut [u8],
) where
    C: BlockDecryptMut + BlockCipher + KeyInit,
{
    let mut cipher = cbc::Decryptor::<C>::new(key, iv);
    for block in data.chunks_exact_mut(C::block_size()) {
        cipher.decrypt_block_mut(GenericArray::from_mut_slice(block));
    }
}

/// Encrypt or decrypt `data` in place under CFB-128 with AES and `key`,
/// 16, 24 or 32 octets.
fn aes_cfb(key: &[u8], iv: &[u8; 16], data: &mut [u8], encrypt: bool) {
    fn run<C>(key: &GenericArray<u8, C::KeySize>, iv: &[u8; 16], data: &mut [u8], encrypt: bool)
    where
        C: BlockEncryptMut + BlockCipher<BlockSize = U16> + KeyInit,
    {
        let iv = GenericArray::from_slice(iv);
        if encrypt {
            cfb_mode::Encryptor::<C>::new(key, iv).encrypt(data);
        } else {
            cfb_mode::Decryptor::<C>::new(key, iv).decrypt(data);
        }
    }
    match key.len() {
        32 => run::<aes::Aes256>(&array::<32>(key).into(), iv, data, encrypt),
        24 => run::<aes::Aes192>(&array::<24>(key).into(), iv, data, encrypt),
        _ => run::<aes::Aes128>(&array::<16>(key).into(), iv, data, encrypt),
    }
}

/// Encrypt or decrypt `data` in place under `protocol` with `key`, the
/// salt `salt`, and the engine's `boots` and `time` the message carries.
fn crypt(
    protocol: PrivProtocol,
    key: &[u8],
    boots: u32,
    time: u32,
    salt: &[u8; 8],
    data: &mut [u8],
    encrypt: bool,
) {
    let iv = iv(protocol, key, boots, time, salt);
    match protocol {
        PrivProtocol::Des => {
            let key = array::<8>(key).into();
            let iv = array::<8>(&iv).into();
            if encrypt {
                cbc_encrypt::<des::Des>(&key, &iv, data);
            } else {
                cbc_decrypt::<des::Des>(&key, &iv, data);
            }
        }
        _ => aes_cfb(key, &iv, data, encrypt),
    }
}

/// Decrypt `cipher_text`, a request's encrypted scoped PDU, into `out`,
/// which is cleared first, with `user`'s privacy key, the request's
/// privacy parameter `priv_params` and the `boots` and `time` it carries,
/// and decode it: a scoped PDU, under DES followed by at most 7 octets of
/// padding.
///
/// # Errors
///
/// [`PrivError::NoKey`] when the user has no privacy key,
/// [`PrivError::BadSalt`] when `priv_params` is not 8 octets,
/// [`PrivError::BadLength`] when a DES ciphertext is not a multiple of 8
/// octets, and [`PrivError::Garbled`] when the plaintext is not a scoped
/// PDU, which is what another key makes.
pub(crate) fn decrypt<'o>(
    user: &LocalizedUser,
    boots: u32,
    time: u32,
    priv_params: &[u8],
    cipher_text: &[u8],
    out: &'o mut Vec<u8>,
) -> Result<ScopedPdu<'o>, PrivError> {
    out.clear();
    let (protocol, key) = user.privacy().ok_or(PrivError::NoKey)?;
    let salt: &[u8; PRIV_PARAMS] = priv_params.try_into().map_err(|_| PrivError::BadSalt)?;
    if protocol == PrivProtocol::Des && !cipher_text.len().is_multiple_of(DES_BLOCK) {
        return Err(PrivError::BadLength);
    }
    out.extend_from_slice(cipher_text);
    crypt(protocol, key, boots, time, salt, out, false);
    let padding = match protocol {
        PrivProtocol::Des => DES_BLOCK - 1,
        _ => 0,
    };
    v3::decode_scoped_pdu(out, padding).map_err(|_| PrivError::Garbled)
}

/// Encrypt `data`, a response's scoped PDU padded to [`padded_len`], in
/// place with `user`'s privacy key, the engine's boots and time the
/// response carries, and the next salt of `salts`, which is written into
/// `priv_params`. Does nothing for a user without privacy.
pub(crate) fn encrypt(
    user: &LocalizedUser,
    engine: &Snapshot<'_>,
    salts: &SaltCounter,
    data: &mut [u8],
    priv_params: &mut [u8; PRIV_PARAMS],
) {
    let Some((protocol, key)) = user.privacy() else {
        return;
    };
    *priv_params = match protocol {
        PrivProtocol::Des => {
            let mut salt = [0u8; PRIV_PARAMS];
            salt[..4].copy_from_slice(&engine.boots.to_be_bytes());
            salt[4..].copy_from_slice(&salts.next_des().to_be_bytes());
            salt
        }
        _ => salts.next_aes().to_be_bytes(),
    };
    crypt(
        protocol,
        key,
        engine.boots,
        engine.time,
        priv_params,
        data,
        true,
    );
}

/// A usmStats counter (RFC 3414 §5), each reported in a `Report` PDU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsmStat {
    /// `usmStatsUnknownEngineIDs`: the request's engine ID is not the
    /// agent's, or empty (discovery).
    UnknownEngineIds,
    /// `usmStatsUnknownUserNames`: no user has the request's name.
    UnknownUserNames,
    /// `usmStatsUnsupportedSecLevels`: the request asks for a security level
    /// other than its user's.
    UnsupportedSecLevels,
    /// `usmStatsWrongDigests`: the request's HMAC is not the user's.
    WrongDigests,
    /// `usmStatsNotInTimeWindows`: the request's boots or time are outside
    /// the time window.
    NotInTimeWindows,
    /// `usmStatsDecryptionErrors`: the request's scoped PDU does not decrypt
    /// with the user's privacy key.
    DecryptionErrors,
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
            UsmStat::DecryptionErrors => &[0x2b, 6, 1, 6, 3, 15, 1, 1, 6, 0],
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
            UsmStat::DecryptionErrors => Counter::UsmDecryptionErrors,
        }
    }
}

/// What [`verify`] decides for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsmVerdict<'u, 'p> {
    /// Answer the scoped PDU for `user`, authenticated with its key when
    /// `authenticated`; `scoped` is the decrypted scoped PDU when the
    /// request was encrypted, and `None` when its scoped PDU is plaintext.
    Answer {
        user: &'u LocalizedUser,
        authenticated: bool,
        scoped: Option<ScopedPdu<'p>>,
    },
    /// Answer with a `Report` of `stat`, authenticated with the request's
    /// user's key when `authenticated`.
    Report { stat: UsmStat, authenticated: bool },
}

/// Whether the SNMPv3 request `msg`, decoded from `raw`, is answered, or
/// reported, by an agent with `engine` and `users` (RFC 3414 §3.2), its
/// scoped PDU decrypted into `plain` when it is encrypted. Pure.
///
/// The checks run in this order, the first that fails deciding:
///
/// 1. The engine ID is empty or another engine's: `UnknownEngineIds`,
///    unauthenticated. This is discovery.
/// 2. No user has the request's name: `UnknownUserNames`.
/// 3. The security level is not the user's: the authentication flag is set
///    for a user without `auth` or clear for one with it, or the privacy
///    flag is set for a user without `priv` or clear for one with it:
///    `UnsupportedSecLevels`. A user's level is its minimum.
/// 4. The HMAC over the whole message, its authentication parameter zeroed,
///    is not the one the user's key makes (compared in constant time), or
///    the parameter is not the protocol's length: `WrongDigests`,
///    unauthenticated.
/// 5. The engine's boots are 2³¹ − 1, the request's boots are not the
///    engine's, or its time is more than 150 seconds from the engine's:
///    `NotInTimeWindows`, authenticated.
/// 6. Under the privacy flag, the scoped PDU does not decrypt with the
///    user's privacy key and the request's privacy parameter: the
///    parameter is not 8 octets, a DES ciphertext is not a multiple of 8
///    octets, the plaintext is not a scoped PDU (under DES followed by at
///    most 7 octets of padding), or its context engine ID is not the
///    engine's: `DecryptionErrors`, authenticated.
///
/// Otherwise the request is answered for its user, authenticated when its
/// authentication flag is set. Every report before step 5 is
/// unauthenticated.
pub(crate) fn verify<'u, 'p>(
    msg: &V3Message<'_>,
    raw: &[u8],
    engine: &Engine,
    users: &'u [LocalizedUser],
    plain: &'p mut Vec<u8>,
) -> UsmVerdict<'u, 'p> {
    let report = |stat, authenticated| UsmVerdict::Report {
        stat,
        authenticated,
    };
    let usm = &msg.usm;
    if usm.engine_id.is_empty() || usm.engine_id != engine.id() {
        return report(UsmStat::UnknownEngineIds, false);
    }
    let Some(user) = user(users, usm.user) else {
        return report(UsmStat::UnknownUserNames, false);
    };
    if (user.auth.is_some(), user.privacy.is_some()) != (msg.flags.auth, msg.flags.privacy) {
        return report(UsmStat::UnsupportedSecLevels, false);
    }
    let Some((protocol, key)) = user.auth() else {
        return UsmVerdict::Answer {
            user,
            authenticated: false,
            scoped: None,
        };
    };
    if !authentic(protocol, key, msg, raw) {
        return report(UsmStat::WrongDigests, false);
    }
    if !engine.in_window(usm.boots, usm.time) {
        return report(UsmStat::NotInTimeWindows, true);
    }
    let Some(encrypted) = msg.encrypted else {
        return UsmVerdict::Answer {
            user,
            authenticated: true,
            scoped: None,
        };
    };
    match decrypt(user, usm.boots, usm.time, usm.priv_params, encrypted, plain) {
        Ok(scoped) if scoped.context_engine_id == engine.id() => UsmVerdict::Answer {
            user,
            authenticated: true,
            scoped: Some(scoped),
        },
        _ => report(UsmStat::DecryptionErrors, true),
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
pub(crate) mod tests {
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
                privacy: None,
            },
            UsmUser {
                name: "watcher".to_string(),
                auth: None,
                privacy: None,
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
    fn request(
        engine: &Engine,
        users: &[LocalizedUser],
        name: &str,
        defects: &[Defect],
    ) -> Vec<u8> {
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
        for defect in defects {
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
        }
        let mut datagram = if defects.contains(&Defect::ShortDigest) {
            client.get_with_auth_len(0x1234, &["1.3.6.1.2.1.1.3.0"], 12)
        } else {
            client.get(0x1234, &["1.3.6.1.2.1.1.3.0"])
        };
        if defects.contains(&Defect::WrongDigest) {
            let msg = v3::decode(&datagram).expect("decodes");
            assert!(!msg.auth_range.is_empty(), "a digest to make wrong");
            let at = msg.auth_range.start;
            datagram[at] ^= 0x01;
        }
        datagram
    }

    /// `verify`'s verdict for `defect` in a request from `name`.
    fn verdict(engine: &Engine, users: &[LocalizedUser], name: &str, defects: &[Defect]) -> String {
        let datagram = request(engine, users, name, defects);
        let msg = v3::decode(&datagram).expect("the request decodes");
        match verify(&msg, &datagram, engine, users, &mut Vec::new()) {
            UsmVerdict::Answer {
                user,
                authenticated,
                ..
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
        assert_eq!(verdict(&engine, &users, name, &[Defect::None]), clean, "the request without its defect");
        assert_eq!(verdict(&engine, &users, name, &[defect]), expected);
    }

    /// A request failing two consecutive checks is decided by the earlier
    /// one, for each pair in `verify`'s order. Guard: the second defect
    /// alone, from a user it applies to, gets its own verdict, so the pair's
    /// verdict is the order's.
    #[rustfmt::skip]
    #[rstest]
    #[case::engine_before_privacy(    "telegraf", [Defect::ForeignEngine,  Defect::Privacy],       "report UnknownEngineIds unauthenticated",     ("telegraf", "report UnsupportedSecLevels unauthenticated"))]
    #[case::user_before_privacy(      "telegraf", [Defect::UnknownUser,    Defect::Privacy],       "report UnknownUserNames unauthenticated",     ("telegraf", "report UnsupportedSecLevels unauthenticated"))]
    #[case::user_before_level(        "watcher",  [Defect::UnknownUser,    Defect::AuthWithoutKey], "report UnknownUserNames unauthenticated",    ("watcher",  "report UnsupportedSecLevels unauthenticated"))]
    #[case::level_before_digest(      "watcher",  [Defect::AuthWithoutKey, Defect::WrongDigest],   "report UnsupportedSecLevels unauthenticated", ("telegraf", "report WrongDigests unauthenticated"))]
    #[case::digest_before_time_window("telegraf", [Defect::WrongDigest,    Defect::Time151Behind], "report WrongDigests unauthenticated",         ("telegraf", "report NotInTimeWindows authenticated"))]
    #[tokio::test(start_paused = true)]
    async fn verify_decides_by_the_first_failing_check(
        #[case] name: &str,
        #[case] defects: [Defect; 2],
        #[case] expected: &str,
        #[case] second_alone: (&str, &str),
    ) {
        let engine = engine();
        tokio::time::advance(std::time::Duration::from_secs(1000)).await;
        let users = users();
        let (second_name, second_verdict) = second_alone;
        assert_eq!(verdict(&engine, &users, second_name, &defects[1..]), second_verdict);
        assert_ne!(second_verdict, expected);
        assert_eq!(verdict(&engine, &users, name, &defects[..1]), expected);
        assert_eq!(verdict(&engine, &users, name, &defects), expected);
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
                verify(&msg, &datagram, engine, &users, &mut Vec::new()),
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
    // ---- privacy --------------------------------------------------------------

    use crate::encoder::snmp::ber::Tag;
    use crate::encoder::snmp::v3::tests::{Client, CAPTURE_ENGINE_ID};
    use crate::sink::snmp::UsmPriv;

    /// A user with `auth` of `lab-auth-pass` and `priv` of `lab-priv-pass`:
    /// what the privacy captures' agent was configured with.
    pub(crate) fn priv_user(name: &str, auth: AuthProtocol, privacy: PrivProtocol) -> UsmUser {
        UsmUser {
            name: name.to_string(),
            auth: Some(UsmAuth {
                protocol: auth,
                password: "lab-auth-pass".to_string(),
            }),
            privacy: Some(UsmPriv {
                protocol: privacy,
                password: "lab-priv-pass".to_string(),
            }),
        }
    }

    /// `snmpget -v3 -l authPriv -u des-sha1 -a SHA -A lab-auth-pass -x DES -X lab-priv-pass -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0`, net-snmp 5.9.4.pre2: its encrypted request, engine time 3.
    pub(crate) const NET_SNMP_DES_GET: &str =
        "308191020103301102044ed2b2c1020300ffe3040107020103043f303d041380001f8804736f6e64\
        613a636170747572653002010102010304086465732d73686131040c1890ca5ceec133c6e7364719\
        0408000000069ad903bc0438ec789e2b8ef5cf03b0db8cfde20bcf2fe9d9f728d1e68c4c44b8c68c\
        3d099ea6b9ec33666253d8168df86cc2fc00fde1501472ab50a0db5c";

    /// This agent's response to [`NET_SNMP_DES_GET`], which snmpget printed as `.1.3.6.1.2.1.1.5.0 = STRING: "rtr-core-01"`.
    pub(crate) const NET_SNMP_DES_RESPONSE: &str =
        "3081a1020103301102044ed2b2c1020300ffe3040103020103043f303d041380001f8804736f6e64\
        613a636170747572653002010102010304086465732d73686131040cac70618709c0810aac938d59\
        040800000001f02af76d0448611247c899eaa66b10e0760112d180a9a4dbef43babeebf19e7d3370\
        73f20b1459d7f4a5f28123a0dbb01b07470c3cf30c701c19001a22922636428a399848d4fe47720c\
        012a8d8c";

    /// `snmpget -v3 -l authPriv -u aes-sha256 -a SHA-256 -A lab-auth-pass -x AES -X lab-priv-pass -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0`, net-snmp 5.9.4.pre2: its encrypted request, engine time 4.
    pub(crate) const NET_SNMP_AES_GET: &str =
        "30819e0201033011020409cd9f94020300ffe3040107020103044d304b041380001f8804736f6e64\
        613a6361707475726530020101020104040a6165732d736861323536041835dbaa4fc469ef091042\
        5b7a0ac47e643df75f2c77935aec0408ea923dc922f33bcb04376ed5e66a89efee77cfd33d45f345\
        73677f91d9b43367ee1d4c9c9b1626fddef1b3aa83e70bb8a82fee4786c63351037edc95d10f6f49\
        bc";

    /// This agent's response to [`NET_SNMP_AES_GET`], which snmpget printed as `.1.3.6.1.2.1.1.5.0 = STRING: "rtr-core-01"`.
    pub(crate) const NET_SNMP_AES_RESPONSE: &str =
        "3081a90201033011020409cd9f94020300ffe3040103020103044d304b041380001f8804736f6e64\
        613a6361707475726530020101020104040a6165732d7368613235360418cd490f63c1b6e6dcb7fe\
        f6c79342ca85b1579b3c3389e8760408a88fa692a86653410442d382dfd5656fcb3c8400b23907e8\
        20206cd9140021c5ff3c4817cc446a54f1b9fd35ddc6cf1a1d11a9bec7bc7437377a53242b861776\
        eb2a4fc805a4b777f566a201";

    /// `snmpget -v3 -l authPriv -u aes192-sha1 -a SHA -A lab-auth-pass -x AES-192 -X lab-priv-pass -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0`, net-snmp 5.9.4.pre2: its encrypted request, engine time 5.
    pub(crate) const NET_SNMP_AES192_GET: &str =
        "308193020103301102043e8158fd020300ffe304010702010304423040041380001f8804736f6e64\
        613a6361707475726530020101020105040b6165733139322d73686131040cc87ea7fd35396faa0a\
        2b5b6d0408d443ec64101707da0437541b535b8ff485bc4fd05a6c34d8e23930a02b3864a783dfbd\
        e35ee23628432d1b52a148facb420e01bc44e77df366c68e089ae34ad3ec";

    /// `snmpget -v3 -l authPriv -u aes192c-sha1 -a SHA -A lab-auth-pass -x AES-192-C -X lab-priv-pass -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0`, net-snmp 5.9.4.pre2: its encrypted request, engine time 6.
    pub(crate) const NET_SNMP_AES192C_GET: &str =
        "308194020103301102045ae1dc0e020300ffe304010702010304433041041380001f8804736f6e64\
        613a6361707475726530020101020106040c616573313932632d73686131040cbf04706a2c00c5f7\
        64caa21204082e06b629b8542f5c04370ef0c4359c7bd1c76fe304b8faec170f413261cd40f2854d\
        9b04e126ba03221043d32aadd828295572d56e995f55c024be76cf70e17e35";

    /// `snmpget -v3 -l authPriv -u aes256-sha1 -a SHA -A lab-auth-pass -x AES-256 -X lab-priv-pass -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0`, net-snmp 5.9.4.pre2: its encrypted request, engine time 7.
    pub(crate) const NET_SNMP_AES256_GET: &str =
        "308193020103301102045ee5121e020300ffe304010702010304423040041380001f8804736f6e64\
        613a6361707475726530020101020107040b6165733235362d73686131040c31a159e00aa613e45b\
        84ebc20408a38bf51b607027ea0437b0ded44b5651bd7d8ff7646a3213c1874272e9e59bd65cbfbc\
        81dcec3ae5ef906d228b3c94fe4f839cdf422069681d70414953423674a3";

    /// `snmpget -v3 -l authPriv -u aes256c-sha1 -a SHA -A lab-auth-pass -x AES-256-C -X lab-priv-pass -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0`, net-snmp 5.9.4.pre2: its encrypted request, engine time 8.
    pub(crate) const NET_SNMP_AES256C_GET: &str =
        "308194020103301102045d942bed020300ffe304010702010304433041041380001f8804736f6e64\
        613a6361707475726530020101020108040c616573323536632d73686131040c38ac7f6444b6bf34\
        d2da3fd90408441b791e31462d6b0437384af11ed95be9a53db611ea49282a9766e1dc1d46c7afa2\
        97dc17ff7e71b8fd93faaf7435bf9898351d326367a7f7734bef95cc904b43";

    /// `snmpget -v3 -l authPriv -u aes-sha256 -a SHA-256 -A lab-auth-pass -x AES -X wrong-priv-pass -On 127.0.0.1:<port> 1.3.6.1.2.1.1.5.0`, net-snmp 5.9.4.pre2: its encrypted request, engine time 9.
    pub(crate) const NET_SNMP_WRONG_PRIV_GET: &str =
        "30819e020103301102042b38fb84020300ffe3040107020103044d304b041380001f8804736f6e64\
        613a6361707475726530020101020109040a6165732d7368613235360418d1126d94a9789ec81507\
        f36283e3731e07d9ef64905307470408905b86348b12da68043795667daa99eb86de0c74cf3a9adb\
        88b00d354c749393fcf541082871cae179f10249b6c7d38fb2c40f6609c523217c5b0a67a706434c\
        68";

    /// This agent's report to [`NET_SNMP_WRONG_PRIV_GET`], `usmStatsDecryptionErrors` 1, authenticated, which snmpget printed as `Decryption error`.
    pub(crate) const NET_SNMP_WRONG_PRIV_REPORT: &str =
        "308194020103301102042b38fb84020300ffe304010102010304453043041380001f8804736f6e64\
        613a6361707475726530020101020109040a6165732d736861323536041874ba26a4d106d34bacea\
        de34425c786bed93d331de47ba7e04003035041380001f8804736f6e64613a636170747572653004\
        00a81c0201000201000201003011300f060a2b060106030f01010600410101";

    /// gosnmp v1.45.0 (Telegraf 1.40.2's) `Get` of `.1.3.6.1.2.1.1.5.0` at `AuthPriv` as aes192-sha1, `SHA`, `AES192`, `lab-auth-pass` and `lab-priv-pass`: its encrypted request, engine time 10.
    pub(crate) const GOSNMP_AES192_GET: &str =
        "30819302010330110204671f3b8d020300ffff04010702010304423040041380001f8804736f6e64\
        613a636170747572653002010102010a040b6165733139322d73686131040c79c08e2279676b79e6\
        fec383040849fcf23a10dadb66043770f40d4636a1793f756f7e272535eebaad03fb71576474af0e\
        822160e325aa4688db23269c937ae36508b13994f729a64331e39a501dc8";

    /// gosnmp v1.45.0 (Telegraf 1.40.2's) `Get` of `.1.3.6.1.2.1.1.5.0` at `AuthPriv` as aes192c-sha1, `SHA`, `AES192C`, `lab-auth-pass` and `lab-priv-pass`: its encrypted request, engine time 11.
    pub(crate) const GOSNMP_AES192C_GET: &str =
        "308194020103301102047e4cfbad020300ffff04010702010304433041041380001f8804736f6e64\
        613a636170747572653002010102010b040c616573313932632d73686131040c1c0fb4bdf1bd890f\
        f158f09f040824e0ec40db04faac04371a08c2dc104751056bb67a28fbe68a82feb3db31334178f1\
        e5ec759058c31d096bd3e312aea3f3924948b62b55f3bc042f89c3764f1d77";

    /// gosnmp v1.45.0 (Telegraf 1.40.2's) `Get` of `.1.3.6.1.2.1.1.5.0` at `AuthPriv` as aes256-sha1, `SHA`, `AES256`, `lab-auth-pass` and `lab-priv-pass`: its encrypted request, engine time 12.
    pub(crate) const GOSNMP_AES256_GET: &str =
        "3081930201033011020433529f4e020300ffff04010702010304423040041380001f8804736f6e64\
        613a636170747572653002010102010c040b6165733235362d73686131040c9d8744b5fd2414cd53\
        0cb1ca04083dc2140a89bf63c604371f52c4418d0d77044fa4187007935f320f17816bdd7841e351\
        b7a03dfe0408e009ad150a101964a6c2cc4c8abc42c5cbe01eb9088b1d85";

    /// gosnmp v1.45.0 (Telegraf 1.40.2's) `Get` of `.1.3.6.1.2.1.1.5.0` at `AuthPriv` as aes256c-sha1, `SHA`, `AES256C`, `lab-auth-pass` and `lab-priv-pass`: its encrypted request, engine time 13.
    pub(crate) const GOSNMP_AES256C_GET: &str =
        "308194020103301102044a69dc3f020300ffff04010702010304433041041380001f8804736f6e64\
        613a636170747572653002010102010d040c616573323536632d73686131040c3e278b48eff89a22\
        5b724c6c04087f23a478fa3eb75904371057a72251683957ea9a6706c889e29c1932d36a8dccaac1\
        1f0c84836974d5d3194c7b701d2eced077aea00e98d362a6f35ab97d886872";

    /// One captured encrypted request: its user's name and protocols, the
    /// request, and the engine time it was made at.
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct PrivCapture {
        pub(crate) user: &'static str,
        pub(crate) auth: AuthProtocol,
        pub(crate) privacy: PrivProtocol,
        pub(crate) request: &'static str,
        pub(crate) time: u32,
    }

    /// Each captured encrypted request but the wrong password's, in the
    /// order they were made: net-snmp's for each of its six `-x` names, then
    /// gosnmp's for the four AES-192 and AES-256 protocols.
    #[rustfmt::skip]
    pub(crate) const PRIV_CAPTURED: [PrivCapture; 10] = [
        PrivCapture { user: "des-sha1",     auth: AuthProtocol::Sha1,   privacy: PrivProtocol::Des,     request: NET_SNMP_DES_GET,     time: 3 },
        PrivCapture { user: "aes-sha256",   auth: AuthProtocol::Sha256, privacy: PrivProtocol::Aes128,  request: NET_SNMP_AES_GET,     time: 4 },
        PrivCapture { user: "aes192-sha1",  auth: AuthProtocol::Sha1,   privacy: PrivProtocol::Aes192,  request: NET_SNMP_AES192_GET,  time: 5 },
        PrivCapture { user: "aes192c-sha1", auth: AuthProtocol::Sha1,   privacy: PrivProtocol::Aes192c, request: NET_SNMP_AES192C_GET, time: 6 },
        PrivCapture { user: "aes256-sha1",  auth: AuthProtocol::Sha1,   privacy: PrivProtocol::Aes256,  request: NET_SNMP_AES256_GET,  time: 7 },
        PrivCapture { user: "aes256c-sha1", auth: AuthProtocol::Sha1,   privacy: PrivProtocol::Aes256c, request: NET_SNMP_AES256C_GET, time: 8 },
        PrivCapture { user: "aes192-sha1",  auth: AuthProtocol::Sha1,   privacy: PrivProtocol::Aes192,  request: GOSNMP_AES192_GET,    time: 10 },
        PrivCapture { user: "aes192c-sha1", auth: AuthProtocol::Sha1,   privacy: PrivProtocol::Aes192c, request: GOSNMP_AES192C_GET,   time: 11 },
        PrivCapture { user: "aes256-sha1",  auth: AuthProtocol::Sha1,   privacy: PrivProtocol::Aes256,  request: GOSNMP_AES256_GET,    time: 12 },
        PrivCapture { user: "aes256c-sha1", auth: AuthProtocol::Sha1,   privacy: PrivProtocol::Aes256c, request: GOSNMP_AES256C_GET,   time: 13 },
    ];

    /// The users of the agent the privacy captures were made against, each
    /// once.
    pub(crate) fn priv_capture_users() -> Vec<UsmUser> {
        PRIV_CAPTURED[..6]
            .iter()
            .map(|capture| priv_user(capture.user, capture.auth, capture.privacy))
            .collect()
    }

    /// `capture`'s user with its keys localized to the captures' engine.
    fn localized(user: &str, auth: AuthProtocol, privacy: PrivProtocol) -> LocalizedUser {
        LocalizedUser::new(&priv_user(user, auth, privacy), &hex(CAPTURE_ENGINE_ID))
    }

    /// `datagram`'s scoped PDU decrypted with `user`'s privacy key: its PDU
    /// type, request-id, context engine ID and varbind names, or the error.
    fn decrypted_pdu(
        datagram: &[u8],
        user: &LocalizedUser,
    ) -> Result<(Tag, i64, Vec<u8>, Vec<String>), PrivError> {
        let msg = v3::decode(datagram).expect("decodes");
        assert!(msg.flags.privacy, "an encrypted message");
        let encrypted = msg.encrypted.expect("the encrypted scoped PDU");
        assert!(!encrypted.is_empty());
        let usm = &msg.usm;
        let mut plain = Vec::new();
        let scoped = decrypt(
            user,
            usm.boots,
            usm.time,
            usm.priv_params,
            encrypted,
            &mut plain,
        )?;
        let names = scoped
            .pdu
            .varbinds()
            .map(|var| {
                let var = var.expect("a complete VarBind");
                let oid = Oid::from_ber_body(var.oid).expect("an OID");
                let value = if var.value_tag == Tag::OctetString.byte() {
                    format!(" = {}", String::from_utf8_lossy(var.value))
                } else {
                    String::new()
                };
                format!("{oid}{value}")
            })
            .collect();
        Ok((
            scoped.pdu.pdu,
            scoped.pdu.request_id,
            scoped.context_engine_id.to_vec(),
            names,
        ))
    }

    /// NIST SP 800-38A F.3.13 (CFB128-AES128.Encrypt), and F.3.14 back.
    #[test]
    fn aes_cfb_128_matches_nist_sp_800_38a_f_3_13() {
        let key = hex("2b7e151628aed2a6abf7158809cf4f3c");
        let iv: [u8; 16] = array(&hex("000102030405060708090a0b0c0d0e0f"));
        let plain = hex(
            "6bc1bee22e409f96e93d7e117393172a ae2d8a571e03ac9c9eb76fac45af8e51 \
             30c81c46a35ce411e5fbc1191a0a52ef f69f2445df4f9b17ad2b417be66c3710",
        );
        let mut data = plain.clone();
        aes_cfb(&key, &iv, &mut data, true);
        assert_eq!(
            data,
            hex(
                "3b3fd92eb72dad20333449f8e83cfb4a c8a64537a0b3a93fcde3cdad9f1ce58b \
                 26751f67a3cbb140b1808cf187a4f4df c04b05357c5d1c0eeac4c66f9ff7f2e6"
            )
        );
        aes_cfb(&key, &iv, &mut data, false);
        assert_eq!(data, plain);
    }

    /// NIST SP 800-38A F.2.1 (CBC-AES128.Encrypt), and F.2.2 back, through
    /// the CBC mode DES uses.
    #[test]
    fn cbc_matches_nist_sp_800_38a_f_2_1() {
        let key = array::<16>(&hex("2b7e151628aed2a6abf7158809cf4f3c")).into();
        let iv = array::<16>(&hex("000102030405060708090a0b0c0d0e0f")).into();
        let plain = hex(
            "6bc1bee22e409f96e93d7e117393172a ae2d8a571e03ac9c9eb76fac45af8e51 \
             30c81c46a35ce411e5fbc1191a0a52ef f69f2445df4f9b17ad2b417be66c3710",
        );
        let mut data = plain.clone();
        cbc_encrypt::<aes::Aes128>(&key, &iv, &mut data);
        assert_eq!(
            data,
            hex(
                "7649abac8119b246cee98e9b12e9197d 5086cb9b507219ee95db113a917678b2 \
                 73bed6b8e3c1743b7116e69e22229516 3ff1caa1681fac09120eca307586e1a7"
            )
        );
        cbc_decrypt::<aes::Aes128>(&key, &iv, &mut data);
        assert_eq!(data, plain);
    }

    /// Each captured encrypted request, from net-snmp for each of its `-x`
    /// names and from gosnmp for the AES-192 and AES-256 protocols, decrypts
    /// with the key its user's passwords make to a GET of sysName.0 for the
    /// captures' engine.
    #[rustfmt::skip]
    #[rstest]
    #[case::net_snmp_des(      0)]
    #[case::net_snmp_aes(      1)]
    #[case::net_snmp_aes_192(  2)]
    #[case::net_snmp_aes_192_c(3)]
    #[case::net_snmp_aes_256(  4)]
    #[case::net_snmp_aes_256_c(5)]
    #[case::gosnmp_aes192(     6)]
    #[case::gosnmp_aes192c(    7)]
    #[case::gosnmp_aes256(     8)]
    #[case::gosnmp_aes256c(    9)]
    fn each_captured_request_decrypts_with_its_users_key(#[case] capture: usize) {
        let PrivCapture { user, auth, privacy, request, time } = PRIV_CAPTURED[capture];
        let datagram = hex(request);
        let msg = v3::decode(&datagram).expect("decodes");
        assert_eq!((msg.usm.user, msg.usm.time), (user.as_bytes(), time));
        assert_eq!(msg.usm.priv_params.len(), PRIV_PARAMS);
        let (pdu, _, context, names) = decrypted_pdu(&datagram, &localized(user, auth, privacy))
            .expect("decrypts");
        assert_eq!(pdu, Tag::GetRequest);
        assert_eq!(context, hex(CAPTURE_ENGINE_ID));
        assert_eq!(names, ["1.3.6.1.2.1.1.5.0"]);
    }

    /// A capture made with one AES-192 or AES-256 key extension does not
    /// decrypt with the other's key: with SHA-1 the key is longer than the
    /// hash, so the extensions differ. Guard: its own extension decrypts it.
    #[rustfmt::skip]
    #[rstest]
    #[case::net_snmp_aes_192(  2, PrivProtocol::Aes192c)]
    #[case::net_snmp_aes_192_c(3, PrivProtocol::Aes192)]
    #[case::net_snmp_aes_256(  4, PrivProtocol::Aes256c)]
    #[case::net_snmp_aes_256_c(5, PrivProtocol::Aes256)]
    #[case::gosnmp_aes192(     6, PrivProtocol::Aes192c)]
    #[case::gosnmp_aes192c(    7, PrivProtocol::Aes192)]
    #[case::gosnmp_aes256(     8, PrivProtocol::Aes256c)]
    #[case::gosnmp_aes256c(    9, PrivProtocol::Aes256)]
    fn the_other_key_extension_does_not_decrypt_a_capture(
        #[case] capture: usize,
        #[case] other: PrivProtocol,
    ) {
        let PrivCapture { user, auth, privacy, request, .. } = PRIV_CAPTURED[capture];
        let datagram = hex(request);
        assert!(decrypted_pdu(&datagram, &localized(user, auth, privacy)).is_ok(), "its own extension");
        assert_eq!(key_len(other), key_len(privacy));
        assert_eq!(
            decrypted_pdu(&datagram, &localized(user, auth, other)),
            Err(PrivError::Garbled)
        );
    }

    /// The responses net-snmp accepted, printing sysName, decrypt with the
    /// user's key to a Response carrying it, and carry their own HMAC; the
    /// DES salt begins with the agent's boots.
    #[rustfmt::skip]
    #[rstest]
    #[case::des(NET_SNMP_DES_RESPONSE, 0)]
    #[case::aes(NET_SNMP_AES_RESPONSE, 1)]
    fn the_responses_net_snmp_accepted_decrypt_to_sys_name(#[case] response: &str, #[case] capture: usize) {
        let PrivCapture { user, auth, privacy, request, .. } = PRIV_CAPTURED[capture];
        let user = localized(user, auth, privacy);
        let datagram = hex(response);
        let msg = v3::decode(&datagram).expect("decodes");
        let request_id = decrypted_pdu(&hex(request), &user).expect("the request decrypts").1;
        let (pdu, id, context, names) = decrypted_pdu(&datagram, &user).expect("decrypts");
        assert_eq!((pdu, id), (Tag::Response, request_id));
        assert_eq!(context, hex(CAPTURE_ENGINE_ID));
        assert_eq!(names, ["1.3.6.1.2.1.1.5.0 = rtr-core-01"]);
        assert!(msg.flags.auth && msg.flags.privacy && !msg.flags.reportable);
        if privacy == PrivProtocol::Des {
            assert_eq!(&msg.usm.priv_params[..4], &1u32.to_be_bytes(), "boots, then the counter");
        }
        let (protocol, key) = user.auth().expect("a key");
        let mut zeroed = datagram.clone();
        zeroed[msg.auth_range.clone()].fill(0);
        let mut digest = vec![0; msg.usm.auth_params.len()];
        hmac(protocol, key, &zeroed, &mut digest);
        assert_eq!(digest, msg.usm.auth_params);
    }

    /// Each privacy key is its cipher's length, and begins with the key the
    /// privacy password localizes to.
    #[rustfmt::skip]
    #[rstest]
    #[case::des_md5(       PrivProtocol::Des,     AuthProtocol::Md5,    16)]
    #[case::des_sha512(    PrivProtocol::Des,     AuthProtocol::Sha512, 16)]
    #[case::aes128_md5(    PrivProtocol::Aes128,  AuthProtocol::Md5,    16)]
    #[case::aes192_md5(    PrivProtocol::Aes192,  AuthProtocol::Md5,    24)]
    #[case::aes192c_sha1(  PrivProtocol::Aes192c, AuthProtocol::Sha1,   24)]
    #[case::aes256_md5(    PrivProtocol::Aes256,  AuthProtocol::Md5,    32)]
    #[case::aes256c_md5(   PrivProtocol::Aes256c, AuthProtocol::Md5,    32)]
    #[case::aes256_sha224( PrivProtocol::Aes256,  AuthProtocol::Sha224, 32)]
    #[case::aes256c_sha384(PrivProtocol::Aes256c, AuthProtocol::Sha384, 32)]
    fn each_privacy_key_is_its_ciphers_length(
        #[case] privacy: PrivProtocol,
        #[case] auth: AuthProtocol,
        #[case] len: usize,
    ) {
        let engine = hex(RFC_ENGINE_ID);
        let kul = localize(auth, b"lab-priv-pass", &engine);
        let key = extend_key(privacy, auth, &kul, &engine);
        assert_eq!((key.len(), key_len(privacy)), (len, len));
        let shared = len.min(kul.len());
        assert_eq!(&key[..shared], &kul[..shared]);
    }

    /// The two extensions agree on the octets the hash gives and differ
    /// after them; with a hash as long as the key there is nothing to extend
    /// and they give one key.
    #[test]
    fn the_extensions_differ_only_past_the_hash() {
        let engine = hex(RFC_ENGINE_ID);
        let extended = |privacy, auth| {
            extend_key(
                privacy,
                auth,
                &localize(auth, b"lab-priv-pass", &engine),
                &engine,
            )
        };
        let blumenthal = extended(PrivProtocol::Aes256, AuthProtocol::Sha1);
        let reeder = extended(PrivProtocol::Aes256c, AuthProtocol::Sha1);
        assert_eq!(blumenthal[..20], reeder[..20]);
        assert_ne!(blumenthal[20..], reeder[20..]);
        assert_eq!(
            extended(PrivProtocol::Aes256, AuthProtocol::Sha256),
            extended(PrivProtocol::Aes256c, AuthProtocol::Sha256)
        );
    }

    /// `decrypt` refuses a privacy parameter that is not 8 octets, a DES
    /// ciphertext that is not whole blocks, and a user without privacy.
    /// Guard: the capture decrypts unedited.
    #[test]
    fn decrypt_refuses_what_cannot_be_decrypted() {
        let PrivCapture {
            user,
            auth,
            privacy,
            request,
            ..
        } = PRIV_CAPTURED[0];
        let datagram = hex(request);
        let msg = v3::decode(&datagram).expect("decodes");
        let user = localized(user, auth, privacy);
        let (usm, encrypted) = (&msg.usm, msg.encrypted.expect("encrypted"));
        let mut plain = Vec::new();
        assert!(decrypt(
            &user,
            usm.boots,
            usm.time,
            usm.priv_params,
            encrypted,
            &mut plain
        )
        .is_ok());
        assert_eq!(
            decrypt(
                &user,
                usm.boots,
                usm.time,
                &usm.priv_params[..7],
                encrypted,
                &mut plain
            ),
            Err(PrivError::BadSalt)
        );
        assert_eq!(
            decrypt(
                &user,
                usm.boots,
                usm.time,
                usm.priv_params,
                &encrypted[..15],
                &mut plain
            ),
            Err(PrivError::BadLength)
        );
        let no_priv = &users()[0];
        assert_eq!(
            decrypt(
                no_priv,
                usm.boots,
                usm.time,
                usm.priv_params,
                encrypted,
                &mut plain
            ),
            Err(PrivError::NoKey)
        );
    }

    /// One defect an encrypted request may carry.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum PrivDefect {
        None,
        WrongPrivPassword,
        DesCiphertextOf15,
        ForeignContext,
        AuthNoPriv,
        StaleAndWrongPrivPassword,
    }

    /// `verify`'s verdict for a GET of sysName.0 from `name`, a user of
    /// [`priv_capture_users`], carrying `defect`, against the captures'
    /// engine 1000 seconds after its bind.
    fn priv_verdict(
        engine: &Engine,
        users: &[LocalizedUser],
        name: &str,
        defect: PrivDefect,
    ) -> String {
        let configured = priv_capture_users()
            .into_iter()
            .find(|user| user.name == name)
            .expect("a capture user");
        let mut client = Client::of(&configured, &engine.snapshot());
        match defect {
            PrivDefect::None => {}
            PrivDefect::WrongPrivPassword | PrivDefect::StaleAndWrongPrivPassword => {
                let mut wrong = configured.clone();
                if let Some(privacy) = &mut wrong.privacy {
                    privacy.password = "wrong-priv-pass".to_string();
                }
                client.cipher = Some(LocalizedUser::new(&wrong, engine.id()));
                if defect == PrivDefect::StaleAndWrongPrivPassword {
                    client.time -= 151;
                }
            }
            PrivDefect::DesCiphertextOf15 => client.tamper = Some(|data| data.truncate(15)),
            PrivDefect::ForeignContext => client.context = Some(hex("80001f8804666f726569676e")),
            PrivDefect::AuthNoPriv => client.privacy = false,
        }
        let datagram = client.get(0x2468, &["1.3.6.1.2.1.1.5.0"]);
        let msg = v3::decode(&datagram).expect("the request decodes");
        let mut plain = Vec::new();
        match verify(&msg, &datagram, engine, users, &mut plain) {
            UsmVerdict::Answer {
                user,
                authenticated,
                scoped,
            } => {
                let scoped = scoped.map(|scoped| {
                    (
                        scoped.pdu.pdu,
                        scoped.pdu.request_id,
                        scoped.pdu.varbinds().count(),
                    )
                });
                format!(
                    "answer {} {authenticated} {scoped:?}",
                    String::from_utf8_lossy(user.name())
                )
            }
            UsmVerdict::Report {
                stat,
                authenticated,
            } => format!("report {stat:?} {authenticated}"),
        }
    }

    /// Each refusal of an encrypted request, in `verify`'s order: a wrong
    /// privacy password, a DES ciphertext of 15 octets and another context
    /// engine are decryption errors, authenticated; authNoPriv from a user
    /// with privacy is an unsupported level. A stale request with a wrong
    /// privacy password is decided by the time window, checked first. Guard:
    /// the same request without its defect is answered with its decrypted
    /// scoped PDU.
    #[rustfmt::skip]
    #[rstest]
    #[case::aes(                          "aes-sha256", PrivDefect::None,                      "answer aes-sha256 true Some((GetRequest, 9320, 1))")]
    #[case::des(                          "des-sha1",   PrivDefect::None,                      "answer des-sha1 true Some((GetRequest, 9320, 1))")]
    #[case::aes256c(                      "aes256c-sha1", PrivDefect::None,                    "answer aes256c-sha1 true Some((GetRequest, 9320, 1))")]
    #[case::wrong_priv_password(          "aes-sha256", PrivDefect::WrongPrivPassword,         "report DecryptionErrors true")]
    #[case::des_wrong_priv_password(      "des-sha1",   PrivDefect::WrongPrivPassword,         "report DecryptionErrors true")]
    #[case::des_ciphertext_of_15_octets(  "des-sha1",   PrivDefect::DesCiphertextOf15,         "report DecryptionErrors true")]
    #[case::foreign_context_engine(       "aes-sha256", PrivDefect::ForeignContext,            "report DecryptionErrors true")]
    #[case::auth_no_priv_to_a_priv_user(  "aes-sha256", PrivDefect::AuthNoPriv,                "report UnsupportedSecLevels false")]
    #[case::time_window_before_decryption("aes-sha256", PrivDefect::StaleAndWrongPrivPassword, "report NotInTimeWindows true")]
    #[tokio::test(start_paused = true)]
    async fn verify_decrypts_or_reports_in_rfc_3414_order(
        #[case] name: &str,
        #[case] defect: PrivDefect,
        #[case] expected: &str,
    ) {
        let engine = Engine::new(hex(CAPTURE_ENGINE_ID), Instant::now());
        tokio::time::advance(std::time::Duration::from_secs(1000)).await;
        let users: Vec<LocalizedUser> = priv_capture_users()
            .iter()
            .map(|user| LocalizedUser::new(user, engine.id()))
            .collect();
        assert_eq!(
            priv_verdict(&engine, &users, name, PrivDefect::None),
            format!("answer {name} true Some((GetRequest, 9320, 1))"),
            "the request without its defect"
        );
        assert_eq!(priv_verdict(&engine, &users, name, defect), expected);
    }

    /// The default engine ID is 21 octets: net-snmp's enterprise prefix,
    /// format 5, and the first 16 octets of the SHA-256 of
    /// `sonda:<host>:<address>` (the expected value computed apart, with
    /// Python's hashlib); it is the same on every call and differs with the
    /// host and with the address.
    #[test]
    fn the_default_engine_id_is_format_5_of_the_host_and_the_address() {
        let address: SocketAddr = "0.0.0.0:1161".parse().expect("an address");
        let id = default_engine_id(b"rtr-a", address);
        assert_eq!(id, hex("80001f88054cbd66c5126359280cd68e04ffd92ae2"));
        assert_eq!(
            (id.len(), &id[..5]),
            (21, &[0x80, 0x00, 0x1f, 0x88, 0x05][..])
        );
        assert_eq!(
            id,
            default_engine_id(b"rtr-a", address),
            "the same on every call"
        );
        assert_ne!(id, default_engine_id(b"rtr-b", address), "another host");
        assert_ne!(
            id,
            default_engine_id(b"rtr-a", "0.0.0.0:1162".parse().expect("an address")),
            "another address"
        );
    }

    /// Debug prints neither key of a user with privacy.
    #[test]
    fn debug_prints_no_privacy_key() {
        let user = localized("aes-sha256", AuthProtocol::Sha256, PrivProtocol::Aes128);
        let debug = format!("{user:?}");
        assert!(
            debug.contains("aes-sha256") && debug.contains("Aes128"),
            "{debug}"
        );
        let (_, key) = user.privacy().expect("a privacy key");
        assert!(!debug.contains(&format!("{key:?}")), "{debug}");
        let (_, key) = user.auth().expect("a key");
        assert!(!debug.contains(&format!("{key:?}")), "{debug}");
    }
}
