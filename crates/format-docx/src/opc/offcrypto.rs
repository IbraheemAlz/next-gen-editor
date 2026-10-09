//! Issue #345 — reading an encrypted (password-protected) package:
//! MS-OFFCRYPTO / ECMA-376 Part 2 "Encryption".
//!
//! An encrypted `.docx` is an OLE compound file ([`super::cfb`]) whose
//! `EncryptionInfo` stream says how the key is derived from the password
//! and whose `EncryptedPackage` stream holds the encrypted ZIP. Two
//! schemes are read:
//!
//! - **Agile** (version 4.4 — every Office since 2010): an XML descriptor;
//!   the password key is `H(salt ‖ UTF-16LE(password))` re-hashed
//!   `spinCount` times as `H(LE32(i) ‖ h)`, then `H(h ‖ blockKey)` per
//!   purpose (verifier input, verifier hash, the wrapped key), truncated
//!   (or `0x36`-padded) to the key size. The password is verified by
//!   decrypting the verifier input and comparing its hash; the wrapped
//!   intermediate key then decrypts the package in 4096-byte segments,
//!   each AES-CBC with IV `H(keyDataSalt ‖ LE32(segment))`. Hashes
//!   SHA-1 / SHA-256 / SHA-384 / SHA-512, AES-128/192/256.
//! - **Standard** (versions 3.2 / 4.2 with the AES flag — Office 2007):
//!   SHA-1, a fixed 50 000 spins, the CryptoAPI key derivation, AES-ECB.
//!
//! RC4 ("CryptoAPI" / binary-format) and extensible encryption are
//! refused as unsupported. The package's HMAC (`dataIntegrity`) is not
//! verified — a tampered package still has to pass the ZIP reader and
//! its CRCs. Decryption only: saving writes an UNENCRYPTED package.
//!
//! Hostile input: the spin count is capped at MS-OFFCRYPTO's own maximum
//! (10 000 000), the declared package size is bounded by the caller's
//! package budget, and every length is checked before slicing.

use aes::cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray};
use quick_xml::events::Event as XmlEvent;
use quick_xml::reader::Reader;
use sha1::Digest;

use super::cfb::CompoundFile;
use crate::error::DocxError;

/// MS-OFFCRYPTO §2.3.4.11: "spinCount … MUST NOT be greater than
/// 10,000,000".
pub const MAX_SPIN_COUNT: u32 = 10_000_000;
/// Agile package segment size.
const SEGMENT: usize = 4096;
/// Agile block keys (MS-OFFCRYPTO §2.3.4.13).
const BLOCK_VERIFIER_INPUT: [u8; 8] = [0xfe, 0xa7, 0xd2, 0x76, 0x3b, 0x4b, 0x9e, 0x79];
const BLOCK_VERIFIER_HASH: [u8; 8] = [0xd7, 0xaa, 0x0f, 0x6d, 0x30, 0x61, 0x34, 0x4e];
const BLOCK_KEY: [u8; 8] = [0x14, 0x6e, 0x0b, 0xe7, 0xab, 0xac, 0xd0, 0xd6];

/// The hash algorithms the agile descriptor may name (MD5 / RIPEMD and
/// the like are refused as unsupported).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlg {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl HashAlg {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name.to_ascii_uppercase().replace('-', "").as_str() {
            "SHA1" => Self::Sha1,
            "SHA256" => Self::Sha256,
            "SHA384" => Self::Sha384,
            "SHA512" => Self::Sha512,
            _ => return None,
        })
    }

    /// `H(parts[0] ‖ parts[1] ‖ …)`.
    fn digest(self, parts: &[&[u8]]) -> Vec<u8> {
        fn run<D: Digest>(parts: &[&[u8]]) -> Vec<u8> {
            let mut d = D::new();
            for p in parts {
                d.update(p);
            }
            d.finalize().to_vec()
        }
        match self {
            Self::Sha1 => run::<sha1::Sha1>(parts),
            Self::Sha256 => run::<sha2::Sha256>(parts),
            Self::Sha384 => run::<sha2::Sha384>(parts),
            Self::Sha512 => run::<sha2::Sha512>(parts),
        }
    }

    /// The iterated password hash: `H0 = H(salt ‖ UTF-16LE(password))`,
    /// `Hn = H(LE32(n - 1) ‖ Hn-1)` for `spin` rounds.
    pub fn password_hash(self, salt: &[u8], password: &str, spin: u32) -> Vec<u8> {
        fn run<D: Digest>(salt: &[u8], pw: &[u8], spin: u32) -> Vec<u8> {
            let mut h = D::new().chain_update(salt).chain_update(pw).finalize();
            for i in 0..spin {
                h = D::new()
                    .chain_update(i.to_le_bytes())
                    .chain_update(&h)
                    .finalize();
            }
            h.to_vec()
        }
        let pw: Vec<u8> = password
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        match self {
            Self::Sha1 => run::<sha1::Sha1>(salt, &pw, spin),
            Self::Sha256 => run::<sha2::Sha256>(salt, &pw, spin),
            Self::Sha384 => run::<sha2::Sha384>(salt, &pw, spin),
            Self::Sha512 => run::<sha2::Sha512>(salt, &pw, spin),
        }
    }
}

/// An AES key of any of the three sizes.
enum AesKey {
    K128(aes::Aes128),
    K192(aes::Aes192),
    K256(aes::Aes256),
}

impl AesKey {
    fn new(key: &[u8]) -> Result<Self, DocxError> {
        let bad = || DocxError::UnsupportedEncryption(format!("{}-bit AES key", key.len() * 8));
        Ok(match key.len() {
            16 => Self::K128(aes::Aes128::new_from_slice(key).map_err(|_| bad())?),
            24 => Self::K192(aes::Aes192::new_from_slice(key).map_err(|_| bad())?),
            32 => Self::K256(aes::Aes256::new_from_slice(key).map_err(|_| bad())?),
            _ => return Err(bad()),
        })
    }

    fn decrypt_block(&self, block: &mut [u8]) {
        let b = GenericArray::from_mut_slice(block);
        match self {
            Self::K128(k) => k.decrypt_block(b),
            Self::K192(k) => k.decrypt_block(b),
            Self::K256(k) => k.decrypt_block(b),
        }
    }

    /// AES-CBC decryption of the whole 16-byte blocks of `data` (a
    /// trailing partial block is dropped).
    fn cbc_decrypt(&self, iv: &[u8], data: &[u8]) -> Vec<u8> {
        let mut prev = [0u8; 16];
        let n = iv.len().min(16);
        prev[..n].copy_from_slice(&iv[..n]);
        let mut out = Vec::with_capacity(data.len() / 16 * 16);
        for chunk in data.chunks_exact(16) {
            let mut block = [0u8; 16];
            block.copy_from_slice(chunk);
            self.decrypt_block(&mut block);
            for (b, p) in block.iter_mut().zip(prev) {
                *b ^= p;
            }
            out.extend_from_slice(&block);
            prev.copy_from_slice(chunk);
        }
        out
    }

    /// AES-ECB decryption of the whole 16-byte blocks of `data`.
    fn ecb_decrypt(&self, data: &[u8]) -> Vec<u8> {
        let mut out = data[..data.len() / 16 * 16].to_vec();
        for block in out.chunks_exact_mut(16) {
            self.decrypt_block(block);
        }
        out
    }
}

/// Standard base64 (RFC 4648) with whitespace ignored; `None` on any other
/// character outside the alphabet.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// `buf` truncated to `len` bytes, or right-padded with `pad` to it.
fn fit(mut buf: Vec<u8>, len: usize, pad: u8) -> Vec<u8> {
    buf.resize(len, pad);
    buf
}

fn malformed(what: &str) -> DocxError {
    DocxError::UnsupportedEncryption(format!("malformed encryption descriptor ({what})"))
}

/// The agile descriptor's `<keyData>` (the package key) and the password
/// `<p:encryptedKey>`.
#[derive(Debug, Default)]
struct AgileDescriptor {
    key_salt: Vec<u8>,
    key_block_size: usize,
    key_bits: usize,
    key_hash: Option<HashAlg>,
    key_cipher_ok: bool,
    pw_spin: u32,
    pw_salt: Vec<u8>,
    pw_block_size: usize,
    pw_key_bits: usize,
    pw_hash: Option<HashAlg>,
    pw_cipher_ok: bool,
    verifier_input: Vec<u8>,
    verifier_hash: Vec<u8>,
    encrypted_key: Vec<u8>,
    has_password_encryptor: bool,
}

fn parse_agile(xml: &[u8]) -> Result<AgileDescriptor, DocxError> {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut d = AgileDescriptor::default();
    loop {
        let evt = reader
            .read_event_into(&mut buf)
            .map_err(|_| malformed("XML"))?;
        let e = match &evt {
            XmlEvent::Start(e) | XmlEvent::Empty(e) => e,
            XmlEvent::Eof => break,
            _ => {
                buf.clear();
                continue;
            }
        };
        let local = e.local_name();
        let local = local.as_ref();
        if local == b"keyData" || local == b"encryptedKey" {
            let is_key = local == b"keyData";
            if !is_key {
                d.has_password_encryptor = true;
            }
            for a in e.attributes().flatten() {
                let Ok(v) = a.unescape_value() else { continue };
                let v = v.trim();
                let num = || v.parse::<usize>().map_err(|_| malformed("number"));
                let b64 = || base64_decode(v).ok_or_else(|| malformed("base64"));
                match (is_key, a.key.local_name().as_ref()) {
                    (true, b"saltValue") => d.key_salt = b64()?,
                    (true, b"blockSize") => d.key_block_size = num()?,
                    (true, b"keyBits") => d.key_bits = num()?,
                    (true, b"hashAlgorithm") => d.key_hash = HashAlg::from_name(v),
                    (true, b"cipherAlgorithm") => d.key_cipher_ok = v == "AES",
                    (false, b"spinCount") => {
                        d.pw_spin = v.parse().map_err(|_| malformed("spinCount"))?
                    }
                    (false, b"saltValue") => d.pw_salt = b64()?,
                    (false, b"blockSize") => d.pw_block_size = num()?,
                    (false, b"keyBits") => d.pw_key_bits = num()?,
                    (false, b"hashAlgorithm") => d.pw_hash = HashAlg::from_name(v),
                    (false, b"cipherAlgorithm") => d.pw_cipher_ok = v == "AES",
                    (false, b"encryptedVerifierHashInput") => d.verifier_input = b64()?,
                    (false, b"encryptedVerifierHashValue") => d.verifier_hash = b64()?,
                    (false, b"encryptedKeyValue") => d.encrypted_key = b64()?,
                    _ => {}
                }
            }
        }
        buf.clear();
    }
    Ok(d)
}

/// The agile scheme: verify `password`, unwrap the package key, decrypt.
fn decrypt_agile(
    info: &[u8],
    package: &[u8],
    password: &str,
    max_len: u64,
) -> Result<Vec<u8>, DocxError> {
    let xml = info.get(8..).ok_or_else(|| malformed("truncated"))?;
    let d = parse_agile(xml)?;
    if !d.has_password_encryptor {
        return Err(DocxError::UnsupportedEncryption(
            "no password key encryptor (certificate encryption)".into(),
        ));
    }
    let (Some(key_hash), Some(pw_hash)) = (d.key_hash, d.pw_hash) else {
        return Err(DocxError::UnsupportedEncryption(
            "hash algorithm other than SHA-1 / SHA-2".into(),
        ));
    };
    if !(d.key_cipher_ok && d.pw_cipher_ok) {
        return Err(DocxError::UnsupportedEncryption(
            "cipher other than AES".into(),
        ));
    }
    if d.pw_spin > MAX_SPIN_COUNT {
        return Err(malformed("spinCount above 10,000,000"));
    }
    let (key_len, pw_key_len) = (d.key_bits / 8, d.pw_key_bits / 8);
    if d.pw_block_size == 0 || d.key_block_size == 0 || d.pw_salt.is_empty() {
        return Err(malformed("block size / salt"));
    }
    let h = pw_hash.password_hash(&d.pw_salt, password, d.pw_spin);
    let derive = |block: &[u8]| fit(pw_hash.digest(&[&h, block]), pw_key_len, 0x36);
    let iv = fit(d.pw_salt.clone(), d.pw_block_size, 0);

    let verifier_input =
        AesKey::new(&derive(&BLOCK_VERIFIER_INPUT))?.cbc_decrypt(&iv, &d.verifier_input);
    let verifier_hash =
        AesKey::new(&derive(&BLOCK_VERIFIER_HASH))?.cbc_decrypt(&iv, &d.verifier_hash);
    let salt_len = d.pw_salt.len().min(verifier_input.len());
    let expected = pw_hash.digest(&[&verifier_input[..salt_len]]);
    if verifier_hash.get(..expected.len()) != Some(expected.as_slice()) {
        return Err(DocxError::WrongPassword);
    }
    let secret = AesKey::new(&derive(&BLOCK_KEY))?.cbc_decrypt(&iv, &d.encrypted_key);
    let secret = secret
        .get(..key_len)
        .ok_or_else(|| malformed("wrapped key"))?;
    let key = AesKey::new(secret)?;

    let (size, data) = package_size(package, max_len)?;
    let mut out = Vec::with_capacity(data.len().min(size + SEGMENT));
    for (i, seg) in data.chunks(SEGMENT).enumerate() {
        if out.len() >= size {
            break;
        }
        let seg_iv = fit(
            key_hash.digest(&[&d.key_salt, &(i as u32).to_le_bytes()]),
            d.key_block_size,
            0x36,
        );
        out.extend_from_slice(&key.cbc_decrypt(&seg_iv, seg));
    }
    finish_package(out, size)
}

/// The standard (Office 2007) scheme: SHA-1, 50 000 spins, AES-ECB.
fn decrypt_standard(
    info: &[u8],
    package: &[u8],
    password: &str,
    max_len: u64,
) -> Result<Vec<u8>, DocxError> {
    let u32_at = |at: usize| -> Result<u32, DocxError> {
        info.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or_else(|| malformed("truncated header"))
    };
    let flags = u32_at(4)?;
    const F_CRYPTOAPI: u32 = 0x04;
    const F_AES: u32 = 0x20;
    if flags & F_CRYPTOAPI == 0 || flags & F_AES == 0 {
        return Err(DocxError::UnsupportedEncryption(
            "RC4 (CryptoAPI) encryption".into(),
        ));
    }
    let header_size = u32_at(8)? as usize;
    let header = 12;
    let alg_id = u32_at(header + 8)?;
    let alg_hash = u32_at(header + 12)?;
    let key_bits = u32_at(header + 16)? as usize;
    if !matches!(alg_id, 0x660E..=0x6610) || !matches!(alg_hash, 0 | 0x8004) {
        return Err(DocxError::UnsupportedEncryption(format!(
            "algorithm {alg_id:#x} / hash {alg_hash:#x}"
        )));
    }
    let verifier = header
        .checked_add(header_size)
        .ok_or_else(|| malformed("header size"))?;
    let salt_size = u32_at(verifier)? as usize;
    if salt_size != 16 {
        return Err(malformed("salt size"));
    }
    let field = |from: usize, len: usize| {
        info.get(from..from + len)
            .ok_or_else(|| malformed("truncated verifier"))
    };
    let salt = field(verifier + 4, 16)?;
    let enc_verifier = field(verifier + 20, 16)?;
    let hash_size = u32_at(verifier + 36)? as usize;
    let enc_hash = field(verifier + 40, 32)?;

    let h = HashAlg::Sha1.password_hash(salt, password, 50_000);
    let h_final = HashAlg::Sha1.digest(&[&h, &0u32.to_le_bytes()]);
    let xor_pad = |pad: u8| {
        let mut b = [pad; 64];
        for (x, y) in b.iter_mut().zip(&h_final) {
            *x ^= y;
        }
        HashAlg::Sha1.digest(&[&b])
    };
    let mut derived = xor_pad(0x36);
    derived.extend_from_slice(&xor_pad(0x5C));
    let key = AesKey::new(
        derived
            .get(..key_bits / 8)
            .ok_or_else(|| malformed("key size"))?,
    )?;
    let v = key.ecb_decrypt(enc_verifier);
    let vh = key.ecb_decrypt(enc_hash);
    let expected = HashAlg::Sha1.digest(&[&v]);
    if hash_size != expected.len() || vh.get(..expected.len()) != Some(expected.as_slice()) {
        return Err(DocxError::WrongPassword);
    }
    let (size, data) = package_size(package, max_len)?;
    finish_package(key.ecb_decrypt(data), size)
}

/// The `EncryptedPackage` stream's declared plaintext size (bounded by
/// `max_len` and by the ciphertext actually present) and its ciphertext.
fn package_size(package: &[u8], max_len: u64) -> Result<(usize, &[u8]), DocxError> {
    let head: [u8; 8] = package
        .get(..8)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| malformed("EncryptedPackage header"))?;
    let size = u64::from_le_bytes(head);
    if size > max_len {
        return Err(DocxError::PackageTooLarge {
            limit: crate::PackageLimit::TotalBytes,
            max: max_len,
            part: Some("EncryptedPackage".into()),
        });
    }
    let data = &package[8..];
    if size > data.len() as u64 {
        return Err(malformed("EncryptedPackage shorter than its declared size"));
    }
    Ok((size as usize, data))
}

fn finish_package(mut out: Vec<u8>, size: usize) -> Result<Vec<u8>, DocxError> {
    if out.len() < size {
        return Err(malformed("EncryptedPackage shorter than its declared size"));
    }
    out.truncate(size);
    Ok(out)
}

/// Issue #345 — decrypt the encrypted package in compound file `cfb` with
/// `password`: the plaintext ZIP, or `WrongPassword` /
/// `UnsupportedEncryption` / `PackageTooLarge` (a declared size past
/// `max_len`).
pub fn decrypt_package(cfb: &[u8], password: &str, max_len: u64) -> Result<Vec<u8>, DocxError> {
    let cf =
        CompoundFile::parse(cfb).map_err(|e| DocxError::UnsupportedEncryption(e.to_string()))?;
    let read = |name: &str| -> Result<Vec<u8>, DocxError> {
        cf.stream(name)
            .ok_or(DocxError::CompoundFile)?
            .map_err(|e| DocxError::UnsupportedEncryption(e.to_string()))
    };
    let info = read("EncryptionInfo")?;
    let package = read("EncryptedPackage")?;
    let major = info.get(..2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let minor = info.get(2..4).map(|b| u16::from_le_bytes([b[0], b[1]]));
    match (major, minor) {
        (Some(4), Some(4)) => decrypt_agile(&info, &package, password, max_len),
        (Some(2..=4), Some(2)) => decrypt_standard(&info, &package, password, max_len),
        (Some(3 | 4), Some(3)) => Err(DocxError::UnsupportedEncryption(
            "extensible encryption".into(),
        )),
        (major, minor) => Err(DocxError::UnsupportedEncryption(format!(
            "EncryptionInfo version {}.{}",
            major.unwrap_or(0),
            minor.unwrap_or(0)
        ))),
    }
}

/// Test support: an agile ENCRYPTOR (AES-CBC + the same key derivation),
/// so fixtures need no binary blob and no Office. Deterministic: the
/// salts and the package key are fixed inputs.
#[doc(hidden)]
pub mod test_encrypt {
    use super::*;
    use aes::cipher::BlockEncrypt;

    fn cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
        let mut padded = data.to_vec();
        padded.resize(data.len().div_ceil(16).max(1) * 16, 0);
        let mut prev = [0u8; 16];
        prev.copy_from_slice(&fit(iv.to_vec(), 16, 0));
        let enc = |b: &mut [u8]| {
            let ga = GenericArray::from_mut_slice(b);
            match key.len() {
                16 => aes::Aes128::new_from_slice(key).unwrap().encrypt_block(ga),
                24 => aes::Aes192::new_from_slice(key).unwrap().encrypt_block(ga),
                _ => aes::Aes256::new_from_slice(key).unwrap().encrypt_block(ga),
            }
        };
        for block in padded.chunks_exact_mut(16) {
            for (b, p) in block.iter_mut().zip(prev) {
                *b ^= p;
            }
            enc(block);
            prev.copy_from_slice(block);
        }
        padded
    }

    fn b64(data: &[u8]) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in data.chunks(3) {
            let n = (u32::from(c[0]) << 16)
                | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                | u32::from(*c.get(2).unwrap_or(&0));
            for i in 0..4 {
                if i <= c.len() {
                    out.push(A[(n >> (18 - 6 * i)) as usize & 63] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// Encrypt `zip` with `password` under agile encryption (AES-256,
    /// `hash`, `spin` rounds) into a compound file Office opens.
    pub fn encrypt_agile(zip: &[u8], password: &str, hash: HashAlg, spin: u32) -> Vec<u8> {
        let hash_name = match hash {
            HashAlg::Sha1 => "SHA1",
            HashAlg::Sha256 => "SHA256",
            HashAlg::Sha384 => "SHA384",
            HashAlg::Sha512 => "SHA512",
        };
        let hash_size = hash.digest(&[]).len();
        let key_salt: Vec<u8> = (0..16u8)
            .map(|i| i.wrapping_mul(17).wrapping_add(3))
            .collect();
        let pw_salt: Vec<u8> = (0..16u8)
            .map(|i| i.wrapping_mul(29).wrapping_add(7))
            .collect();
        let secret: Vec<u8> = (0..32u8)
            .map(|i| i.wrapping_mul(13).wrapping_add(5))
            .collect();
        let verifier: Vec<u8> = (0..16u8)
            .map(|i| i.wrapping_mul(7).wrapping_add(1))
            .collect();

        let h = hash.password_hash(&pw_salt, password, spin);
        let derive = |block: &[u8]| fit(hash.digest(&[&h, block]), 32, 0x36);
        let enc_input = cbc_encrypt(&derive(&BLOCK_VERIFIER_INPUT), &pw_salt, &verifier);
        let enc_hash = cbc_encrypt(
            &derive(&BLOCK_VERIFIER_HASH),
            &pw_salt,
            &hash.digest(&[&verifier]),
        );
        let enc_key = cbc_encrypt(&derive(&BLOCK_KEY), &pw_salt, &secret);

        let mut package = (zip.len() as u64).to_le_bytes().to_vec();
        for (i, seg) in zip.chunks(SEGMENT).enumerate() {
            let iv = fit(
                hash.digest(&[&key_salt, &(i as u32).to_le_bytes()]),
                16,
                0x36,
            );
            package.extend_from_slice(&cbc_encrypt(&secret, &iv, seg));
        }
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
             <encryption xmlns=\"http://schemas.microsoft.com/office/2006/encryption\" \
             xmlns:p=\"http://schemas.microsoft.com/office/2006/keyEncryptor/password\">\
             <keyData saltSize=\"16\" blockSize=\"16\" keyBits=\"256\" hashSize=\"{hash_size}\" \
             cipherAlgorithm=\"AES\" cipherChaining=\"ChainingModeCBC\" hashAlgorithm=\"{hash_name}\" \
             saltValue=\"{ks}\"/>\
             <keyEncryptors><keyEncryptor uri=\"http://schemas.microsoft.com/office/2006/keyEncryptor/password\">\
             <p:encryptedKey spinCount=\"{spin}\" saltSize=\"16\" blockSize=\"16\" keyBits=\"256\" \
             hashSize=\"{hash_size}\" cipherAlgorithm=\"AES\" cipherChaining=\"ChainingModeCBC\" \
             hashAlgorithm=\"{hash_name}\" saltValue=\"{ps}\" encryptedVerifierHashInput=\"{vi}\" \
             encryptedVerifierHashValue=\"{vh}\" encryptedKeyValue=\"{ek}\"/>\
             </keyEncryptor></keyEncryptors></encryption>",
            ks = b64(&key_salt),
            ps = b64(&pw_salt),
            vi = b64(&enc_input),
            vh = b64(&enc_hash),
            ek = b64(&enc_key),
        );
        let mut info = vec![0x04, 0x00, 0x04, 0x00, 0x40, 0x00, 0x00, 0x00];
        info.extend_from_slice(xml.as_bytes());
        super::super::cfb::test_writer::build(&[
            ("EncryptionInfo", &info),
            ("EncryptedPackage", &package),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_known_vectors() {
        assert_eq!(base64_decode("TWFu").unwrap(), b"Man");
        assert_eq!(base64_decode("TWE=").unwrap(), b"Ma");
        assert_eq!(base64_decode("TQ==").unwrap(), b"M");
        assert_eq!(base64_decode(" T W\nFu ").unwrap(), b"Man");
        assert!(base64_decode("TW*u").is_none());
    }

    #[test]
    fn agile_round_trip_and_wrong_password() {
        let zip = crate::test_fixtures::docx_with_body("<w:p><w:r><w:t>secret</w:t></w:r></w:p>");
        for hash in [HashAlg::Sha1, HashAlg::Sha512] {
            let cfb = test_encrypt::encrypt_agile(&zip, "pass", hash, 1000);
            assert_eq!(decrypt_package(&cfb, "pass", u64::MAX).unwrap(), zip);
            assert!(matches!(
                decrypt_package(&cfb, "wrong", u64::MAX),
                Err(DocxError::WrongPassword)
            ));
            assert!(matches!(
                decrypt_package(&cfb, "pass", 100),
                Err(DocxError::PackageTooLarge { .. })
            ));
        }
    }

    /// The Apache POI password fixtures (Office-written, local corpus only —
    /// skipped when absent): `bug53475-password-is-pass.docx` (agile) opens
    /// with `pass`, `…-solrcell.docx` with `solrcell`; a wrong password is
    /// `WrongPassword`. Also prints the native spin cost.
    #[test]
    fn office_written_password_fixtures_decrypt() {
        let dir = "/data/corpus/files/apache-poi-test-data-document";
        for (name, pw) in [
            ("bug53475-password-is-pass.docx", "pass"),
            ("bug53475-password-is-solrcell.docx", "solrcell"),
        ] {
            let Ok(bytes) = std::fs::read(format!("{dir}/{name}")) else {
                continue;
            };
            let t = std::time::Instant::now();
            let zip = decrypt_package(&bytes, pw, u64::MAX).expect(name);
            eprintln!("{name}: decrypted {} bytes in {:?}", zip.len(), t.elapsed());
            assert!(zip.starts_with(b"PK\x03\x04"), "{name}");
            assert!(matches!(
                decrypt_package(&bytes, "nope", u64::MAX),
                Err(DocxError::WrongPassword)
            ));
        }
    }

    /// Reference cost of the key derivation Office uses (100 000 spins):
    /// `cargo test -p format-docx --release --lib spin_cost -- --ignored
    /// --nocapture`.
    #[test]
    #[ignore]
    fn spin_cost_reference() {
        for hash in [HashAlg::Sha1, HashAlg::Sha256, HashAlg::Sha512] {
            let t = std::time::Instant::now();
            let h = hash.password_hash(&[7u8; 16], "pass", 100_000);
            eprintln!(
                "{hash:?} x 100000 spins: {:?} ({} bytes)",
                t.elapsed(),
                h.len()
            );
        }
    }

    /// Hostile descriptors: a spin count past the spec maximum and
    /// truncated / garbage streams are refused without panicking.
    #[test]
    fn hostile_descriptors_are_refused() {
        let zip = crate::test_fixtures::docx_with_body("<w:p/>");
        let cfb = test_encrypt::encrypt_agile(&zip, "pass", HashAlg::Sha1, 10);
        let cf = CompoundFile::parse(&cfb).unwrap();
        let info = cf.stream("EncryptionInfo").unwrap().unwrap();
        let package = cf.stream("EncryptedPackage").unwrap().unwrap();
        let text = String::from_utf8_lossy(&info[8..])
            .replace("spinCount=\"10\"", "spinCount=\"4000000000\"");
        let mut huge = info[..8].to_vec();
        huge.extend_from_slice(text.as_bytes());
        assert!(decrypt_agile(&huge, &package, "pass", u64::MAX).is_err());
        for cut in [0, 4, 8, 20, 200, info.len() - 1] {
            let _ = decrypt_agile(&info[..cut], &package, "pass", u64::MAX);
            let _ = decrypt_standard(&info[..cut], &package, "pass", u64::MAX);
        }
        for cut in [0, 7, 8, 9, 100] {
            let _ = decrypt_agile(&info, &package[..cut.min(package.len())], "pass", u64::MAX);
        }
    }
}
