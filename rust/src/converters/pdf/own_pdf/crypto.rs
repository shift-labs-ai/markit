//! Standard PDF security handler: RC4/AESV2 revisions 2–4 and
//! AES-256 revisions 5–6, including user/owner password routes and
//! per-object stream keys.

use anyhow::{anyhow, bail, Result};

use super::document::Pdf;
use super::values::{dget, Dict, Val};

//
// Standard security handler. The password comes from MARKIT_PDF_PASSWORD
// (empty when unset — the common "encrypted for permissions" case). Both
// the user- and owner-password routes are tried.

pub(crate) struct Decryptor {
    key: [u8; 32],
}

impl<'a> Pdf<'a> {
    pub(super) fn setup_decryption(&self) -> Result<()> {
        let Some(enc) = dget(&self.trailer, b"Encrypt") else {
            return Ok(());
        };
        let password = std::env::var("MARKIT_PDF_PASSWORD").unwrap_or_default();
        let pw: &[u8] = password.as_bytes();
        let pw = &pw[..pw.len().min(127)];
        let Val::Dict(enc) = self.resolve(enc)? else {
            bail!("bad Encrypt");
        };
        let g = |key: &[u8]| self.dict_get(&enc, key);

        if !matches!(g(b"Filter")?, Some(Val::Name(b"Standard"))) {
            bail!("non-standard security handler");
        }
        let v = g(b"V")?.and_then(|v| v.as_num()).unwrap_or(0.0) as i64;
        let r = g(b"R")?.and_then(|v| v.as_num()).unwrap_or(0.0) as i64;
        if matches!(v, 1 | 2 | 4) && matches!(r, 2..=4) {
            return self.setup_legacy(&enc, v, r);
        }
        if v != 5 || !(r == 5 || r == 6) {
            bail!("unsupported encryption V={v} R={r}");
        }
        // Stream crypt filter must be AESV3 (or Identity = nothing to do).
        match g(b"StmF")? {
            None | Some(Val::Name(b"Identity")) => return Ok(()),
            Some(Val::Name(b"StdCF")) => {}
            _ => bail!("unsupported StmF"),
        }
        if let Some(Val::Dict(cf)) = g(b"CF")? {
            if let Some(Val::Dict(stdcf)) = self.dict_get(&cf, b"StdCF")? {
                match self.dict_get(&stdcf, b"CFM")? {
                    Some(Val::Name(b"AESV3")) => {}
                    other => bail!("unsupported CFM {other:?}"),
                }
            }
        }

        let get_str = |k: &[u8]| -> Result<Vec<u8>> {
            match self.dict_get(&enc, k)? {
                Some(Val::Str(s)) => Ok(s),
                _ => bail!("missing {}", String::from_utf8_lossy(k)),
            }
        };
        let u = get_str(b"U")?;
        let ue = get_str(b"UE")?;
        let o = get_str(b"O")?;
        let oe = get_str(b"OE")?;
        if u.len() < 48 || o.len() < 48 || ue.len() < 32 || oe.len() < 32 {
            bail!("short U/O/UE/OE");
        }

        // USER password route (ISO 32000-2, 7.6.4.3.3/4).
        if hash_2b(pw, &u[32..40], b"", r) == u[0..32] {
            let ik = hash_2b(pw, &u[40..48], b"", r);
            let key = aes256_cbc_nopad_decrypt(&ik, &[0u8; 16], &ue[..32])?;
            *self.decrypt.borrow_mut() = Some(Decryptor {
                key: key.try_into().map_err(|_| anyhow!("bad UE"))?,
            });
            return Ok(());
        }
        // OWNER password route (uses U as extra hash data).
        if hash_2b(pw, &o[32..40], &u[..48], r) == o[0..32] {
            let ik = hash_2b(pw, &o[40..48], &u[..48], r);
            let key = aes256_cbc_nopad_decrypt(&ik, &[0u8; 16], &oe[..32])?;
            *self.decrypt.borrow_mut() = Some(Decryptor {
                key: key.try_into().map_err(|_| anyhow!("bad OE"))?,
            });
            return Ok(());
        }
        bail!("password required");
    }

    /// Public shim for image extraction.
    pub fn decrypt_stream_pub(&self, raw: &[u8]) -> Result<Vec<u8>> {
        self.decrypt_stream(raw)
    }

    /// Legacy (V<5) key schedule: Algorithm 2 with the empty user password.
    pub(super) fn setup_legacy(&self, enc: &Dict<'_>, v: i64, r: i64) -> Result<()> {
        let get_str = |k: &[u8]| -> Result<Vec<u8>> {
            match self.dict_get(enc, k)? {
                Some(Val::Str(s)) => Ok(s),
                _ => bail!("missing {}", String::from_utf8_lossy(k)),
            }
        };
        let o = get_str(b"O")?;
        let u = get_str(b"U")?;
        let p = self
            .dict_get(enc, b"P")?
            .and_then(|x| x.as_num())
            .unwrap_or(0.0) as i64;
        let length_bits = self
            .dict_get(enc, b"Length")?
            .and_then(|x| x.as_num())
            .unwrap_or(40.0) as usize;
        let key_len = if v == 1 {
            5
        } else {
            (length_bits / 8).clamp(5, 16)
        };

        let mut aes = false;
        if v == 4 {
            match self.dict_get(enc, b"StmF")? {
                None | Some(Val::Name(b"Identity")) => return Ok(()),
                Some(Val::Name(b"StdCF")) => {}
                _ => bail!("unsupported StmF"),
            }
            if let Some(Val::Dict(cf)) = self.dict_get(enc, b"CF")? {
                if let Some(Val::Dict(stdcf)) = self.dict_get(&cf, b"StdCF")? {
                    match self.dict_get(&stdcf, b"CFM")? {
                        Some(Val::Name(b"AESV2")) => aes = true,
                        Some(Val::Name(b"V2")) => {}
                        other => bail!("unsupported CFM {other:?}"),
                    }
                }
            }
        }
        let encrypt_metadata = !matches!(
            self.dict_get(enc, b"EncryptMetadata")?,
            Some(Val::Bool(false))
        );

        // First file ID from the trailer.
        let id0: Vec<u8> = match dget(&self.trailer, b"ID").map(|x| self.resolve(x)) {
            Some(Ok(Val::Array(a))) => match a.first().map(|x| self.resolve(x)) {
                Some(Ok(Val::Str(s))) => s,
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };

        // Algorithm 2 with the (padded) user password.
        let password = std::env::var("MARKIT_PDF_PASSWORD").unwrap_or_default();
        let pw = password.as_bytes();
        let mut padded = [0u8; 32];
        let n = pw.len().min(32);
        padded[..n].copy_from_slice(&pw[..n]);
        padded[n..].copy_from_slice(&PAD[..32 - n]);
        let mut seed = Vec::with_capacity(128);
        seed.extend_from_slice(&padded);
        seed.extend_from_slice(&o[..32.min(o.len())]);
        seed.extend_from_slice(&(p as i32).to_le_bytes());
        seed.extend_from_slice(&id0);
        if r >= 4 && !encrypt_metadata {
            seed.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        }
        let mut key = md5(&seed)[..key_len].to_vec();
        if r >= 3 {
            for _ in 0..50 {
                key = md5(&key)[..key_len].to_vec();
            }
        }

        // Validate against /U (Algorithm 6): tolerate producers that get
        // the padding wrong by comparing only the first 16 bytes for R>=3.
        let ok = if r == 2 {
            rc4(&key, &PAD) == u[..32.min(u.len())]
        } else {
            let mut h = Vec::with_capacity(64);
            h.extend_from_slice(&PAD);
            h.extend_from_slice(&id0);
            let mut x = md5(&h).to_vec();
            for i in 1..=19u8 {
                let k2: Vec<u8> = key.iter().map(|&b| b ^ i).collect();
                x = rc4(&k2, &x);
            }
            let x = rc4(&key, &x);
            u.len() >= 16 && x[..16] == u[..16]
        };
        if ok {
            *self.legacy.borrow_mut() = Some(LegacyCrypt { key, aes });
            return Ok(());
        }

        // Owner-password route (Algorithm 7): recover the user password
        // by decrypting /O with the owner key, then rerun Algorithm 2.
        let mut oseed = Vec::with_capacity(96);
        oseed.extend_from_slice(&padded);
        let mut okey = md5(&oseed)[..key_len].to_vec();
        if r >= 3 {
            for _ in 0..50 {
                okey = md5(&okey)[..key_len].to_vec();
            }
        }
        let mut user_pw = o[..32.min(o.len())].to_vec();
        if r == 2 {
            user_pw = rc4(&okey, &user_pw);
        } else {
            for i in (0..=19u8).rev() {
                let k2: Vec<u8> = okey.iter().map(|&b| b ^ i).collect();
                user_pw = rc4(&k2, &user_pw);
            }
        }
        let mut seed2 = Vec::with_capacity(128);
        seed2.extend_from_slice(&user_pw[..32.min(user_pw.len())]);
        seed2.extend_from_slice(&o[..32.min(o.len())]);
        seed2.extend_from_slice(&(p as i32).to_le_bytes());
        seed2.extend_from_slice(&id0);
        if r >= 4 && !encrypt_metadata {
            seed2.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        }
        let mut key2 = md5(&seed2)[..key_len].to_vec();
        if r >= 3 {
            for _ in 0..50 {
                key2 = md5(&key2)[..key_len].to_vec();
            }
        }
        let ok2 = if r == 2 {
            rc4(&key2, &PAD) == u[..32.min(u.len())]
        } else {
            let mut h = Vec::with_capacity(64);
            h.extend_from_slice(&PAD);
            h.extend_from_slice(&id0);
            let mut x = md5(&h).to_vec();
            for i in 1..=19u8 {
                let k2: Vec<u8> = key2.iter().map(|&b| b ^ i).collect();
                x = rc4(&k2, &x);
            }
            let x = rc4(&key2, &x);
            u.len() >= 16 && x[..16] == u[..16]
        };
        if !ok2 {
            bail!("password required");
        }
        *self.legacy.borrow_mut() = Some(LegacyCrypt { key: key2, aes });
        Ok(())
    }

    /// Legacy per-object stream decryption with caching.
    pub(crate) fn legacy_decrypt<'s>(
        &'s self,
        num: u32,
        generation: u16,
        raw: &[u8],
    ) -> Result<&'s [u8]> {
        if self.legacy_cache.get(&num).is_none() {
            let legacy = self.legacy.borrow();
            let lc = legacy.as_ref().expect("legacy crypt");
            let plain = lc.decrypt_object(num, generation, raw)?;
            self.legacy_cache.insert(num, plain.into_boxed_slice());
        }
        Ok(self.legacy_cache.get(&num).unwrap())
    }

    pub(crate) fn decrypt_stream(&self, raw: &[u8]) -> Result<Vec<u8>> {
        let decrypt = self.decrypt.borrow();
        let Some(d) = decrypt.as_ref() else {
            return Ok(raw.to_vec());
        };
        if raw.len() < 16 {
            bail!("encrypted stream too short");
        }
        let mut out = aes256_cbc_nopad_decrypt(&d.key, &raw[..16], &raw[16..])?;
        // PKCS#7 unpadding (tolerant: some producers pad wrong).
        if let Some(&pad) = out.last() {
            if pad >= 1 && pad as usize <= 16 && pad as usize <= out.len() {
                let n = out.len() - pad as usize;
                if out[n..].iter().all(|&b| b == pad) {
                    out.truncate(n);
                }
            }
        }
        Ok(out)
    }
}

/// Legacy standard security handler state (V1/V2 RC4, V4 RC4/AESV2).
pub(crate) struct LegacyCrypt {
    key: Vec<u8>,
    aes: bool,
}

const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// Tiny MD5 (RFC 1321) — enough for the legacy key schedule.
fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: [u32; 64] =
        std::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32);
    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);

    let mut msg = data.to_vec();
    let bitlen = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while !msg.len().is_multiple_of(64) {
        if msg.len() % 64 == 56 {
            break;
        }
        msg.push(0);
    }
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_le_bytes());

    for chunk in msg.chunks_exact(64) {
        let m: [u32; 16] = std::array::from_fn(|i| {
            u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ])
        });
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(k[i])
                    .wrapping_add(m[g])
                    .rotate_left(S[i]),
            );
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..].copy_from_slice(&d0.to_le_bytes());
    out
}

fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|&b| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            b ^ s[(s[i as usize].wrapping_add(s[j as usize])) as usize]
        })
        .collect()
}

impl LegacyCrypt {
    /// Per-object key (Algorithm 1) + decrypt.
    fn decrypt_object(&self, num: u32, gen: u16, data: &[u8]) -> Result<Vec<u8>> {
        let mut seed = self.key.clone();
        seed.extend_from_slice(&num.to_le_bytes()[..3]);
        seed.extend_from_slice(&gen.to_le_bytes()[..2]);
        if self.aes {
            seed.extend_from_slice(b"sAlT");
        }
        let h = md5(&seed);
        let klen = (self.key.len() + 5).min(16);
        let obj_key = &h[..klen];

        if self.aes {
            if data.len() < 16 || !data.len().is_multiple_of(16) {
                bail!("bad AESV2 stream");
            }
            use aes::cipher::{BlockDecryptMut, KeyIvInit};
            type Dec = cbc::Decryptor<aes::Aes128>;
            let mut buf = data[16..].to_vec();
            let mut dec = Dec::new(obj_key.into(), data[..16].into());
            for chunk in buf.chunks_exact_mut(16) {
                dec.decrypt_block_mut(chunk.into());
            }
            if let Some(&pad) = buf.last() {
                if pad >= 1 && pad as usize <= 16 && pad as usize <= buf.len() {
                    let n = buf.len() - pad as usize;
                    if buf[n..].iter().all(|&b| b == pad) {
                        buf.truncate(n);
                    }
                }
            }
            Ok(buf)
        } else {
            Ok(rc4(obj_key, data))
        }
    }
}

/// ISO 32000-2 Algorithm 2.B (revision 6 hardened hash; revision 5 is a
/// single SHA-256).
fn hash_2b(pw: &[u8], salt: &[u8], udata: &[u8], r: i64) -> [u8; 32] {
    use sha2::{Digest, Sha256, Sha384, Sha512};

    let mut k: Vec<u8> = {
        let mut h = Sha256::new();
        h.update(pw);
        h.update(salt);
        h.update(udata);
        h.finalize().to_vec()
    };
    if r == 5 {
        return k[..32].try_into().unwrap();
    }

    let mut round = 0usize;
    loop {
        // K1 = (pw ‖ K ‖ udata) × 64
        let unit_len = pw.len() + k.len() + udata.len();
        let mut k1 = Vec::with_capacity(unit_len * 64);
        for _ in 0..64 {
            k1.extend_from_slice(pw);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(udata);
        }
        let e = aes128_cbc_nopad_encrypt(&k[..16], &k[16..32], &k1);
        let sum: u32 = e[..16].iter().map(|&b| b as u32).sum();
        k = match sum % 3 {
            0 => Sha256::digest(&e).to_vec(),
            1 => Sha384::digest(&e).to_vec(),
            _ => Sha512::digest(&e).to_vec(),
        };
        round += 1;
        if round >= 64 && (*e.last().unwrap() as usize) <= round - 32 {
            break;
        }
    }
    k[..32].try_into().unwrap()
}

fn aes128_cbc_nopad_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    use aes::cipher::{BlockEncryptMut, KeyIvInit};
    type Enc = cbc::Encryptor<aes::Aes128>;
    let mut buf = data.to_vec();
    let mut enc = Enc::new(key.into(), iv.into());
    for chunk in buf.chunks_exact_mut(16) {
        enc.encrypt_block_mut(chunk.into());
    }
    buf
}

fn aes256_cbc_nopad_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit};
    type Dec = cbc::Decryptor<aes::Aes256>;
    if !data.len().is_multiple_of(16) {
        bail!("ciphertext not block-aligned");
    }
    let mut buf = data.to_vec();
    let mut dec = Dec::new(key.into(), iv.into());
    for chunk in buf.chunks_exact_mut(16) {
        dec.decrypt_block_mut(chunk.into());
    }
    Ok(buf)
}
