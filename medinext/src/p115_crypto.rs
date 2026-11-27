use aes::cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;
use anyhow::{bail, ensure, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use sha1::{Digest, Sha1};

const CRC_SALT: &[u8] = b"^j>WD3Kr?J2gLFjD4W2y@";
const MD5_SALT: &[u8] = b"Qclm8MGWUv59TnrR0XPg";
const G_KEY_L: &[u8] = b"x\x06\xadL3\x86]\x18L\x01?F";
const RSA_KEY: &[u8] = &[0x8d, 0xa5, 0xa5, 0x8d];

const AES_PUBKEY: [u8; 30] = [
    0x1d, 0x03, 0x0e, 0x80, 0xa1, 0x78, 0xdc, 0xee, 0xce, 0xcd, 0xa3, 0x77, 0xde, 0x12, 0x8d, 0x8e,
    0xd9, 0xdd, 0xcf, 0x55, 0xae, 0x61, 0xed, 0x46, 0xea, 0x12, 0x1a, 0x1c, 0xfc, 0x81,
];
const AES_KEY: [u8; 16] = [
    0xfb, 0x1a, 0x19, 0xd6, 0x52, 0xf5, 0xaa, 0xf7, 0xbc, 0x65, 0x1d, 0x0f, 0x69, 0xbf, 0x42, 0x2f,
];
const AES_IV: [u8; 16] = [
    0x69, 0xbf, 0x42, 0x2f, 0x49, 0x96, 0x05, 0x50, 0xa0, 0xad, 0x44, 0xec, 0x34, 0x46, 0xcb, 0x4c,
];

const G_KTS: [u8; 144] = [
    0xf0, 0xe5, 0x69, 0xae, 0xbf, 0xdc, 0xbf, 0x8a, 0x1a, 0x45, 0xe8, 0xbe, 0x7d, 0xa6, 0x73, 0xb8,
    0xde, 0x8f, 0xe7, 0xc4, 0x45, 0xda, 0x86, 0xc4, 0x9b, 0x64, 0x8b, 0x14, 0x6a, 0xb4, 0xf1, 0xaa,
    0x38, 0x01, 0x35, 0x9e, 0x26, 0x69, 0x2c, 0x86, 0x00, 0x6b, 0x4f, 0xa5, 0x36, 0x34, 0x62, 0xa6,
    0x2a, 0x96, 0x68, 0x18, 0xf2, 0x4a, 0xfd, 0xbd, 0x6b, 0x97, 0x8f, 0x4d, 0x8f, 0x89, 0x13, 0xb7,
    0x6c, 0x8e, 0x93, 0xed, 0x0e, 0x0d, 0x48, 0x3e, 0xd7, 0x2f, 0x88, 0xd8, 0xfe, 0xfe, 0x7e, 0x86,
    0x50, 0x95, 0x4f, 0xd1, 0xeb, 0x83, 0x26, 0x34, 0xdb, 0x66, 0x7b, 0x9c, 0x7e, 0x9d, 0x7a, 0x81,
    0x32, 0xea, 0xb6, 0x33, 0xde, 0x3a, 0xa9, 0x59, 0x34, 0x66, 0x3b, 0xaa, 0xba, 0x81, 0x60, 0x48,
    0xb9, 0xd5, 0x81, 0x9c, 0xf8, 0x6c, 0x84, 0x77, 0xff, 0x54, 0x78, 0x26, 0x5f, 0xbe, 0xe8, 0x1e,
    0x36, 0x9f, 0x34, 0x80, 0x5c, 0x45, 0x2c, 0x9b, 0x76, 0xd5, 0x1b, 0x8f, 0xcc, 0xc3, 0xb8, 0xf5,
];

/// 115 m115 1024-bit RSA public modulus (big-endian bytes)
const RSA_MODULUS_BE: [u8; 128] = [
    0x86, 0x86, 0x98, 0x0c, 0x0f, 0x5a, 0x24, 0xc4, 0xb9, 0xd4, 0x30, 0x20, 0xcd, 0x2c, 0x22, 0x70,
    0x3f, 0xf3, 0xf4, 0x50, 0x75, 0x65, 0x29, 0x05, 0x8b, 0x1c, 0xf8, 0x8f, 0x09, 0xb8, 0x60, 0x21,
    0x36, 0x47, 0x71, 0x98, 0xa6, 0xe2, 0x68, 0x31, 0x49, 0x65, 0x9b, 0xd1, 0x22, 0xc3, 0x35, 0x92,
    0xfd, 0xb5, 0xad, 0x47, 0x94, 0x4a, 0xd1, 0xea, 0x4d, 0x36, 0xc6, 0xb1, 0x72, 0xaa, 0xd6, 0x33,
    0x8c, 0x3b, 0xb6, 0xac, 0x62, 0x27, 0x50, 0x2d, 0x01, 0x09, 0x93, 0xac, 0x96, 0x7d, 0x1a, 0xef,
    0x00, 0xf0, 0xc8, 0xe0, 0x38, 0xde, 0x2e, 0x4d, 0x3b, 0xc2, 0xec, 0x36, 0x8a, 0xf2, 0xe9, 0xf1,
    0x0a, 0x6f, 0x1e, 0xda, 0x4f, 0x72, 0x62, 0xf1, 0x36, 0x42, 0x0c, 0x07, 0xc3, 0x31, 0xb8, 0x71,
    0xbf, 0x13, 0x9f, 0x74, 0xf3, 0x01, 0x0e, 0x3c, 0x4f, 0xe5, 0x7d, 0xf3, 0xaf, 0xb7, 0x16, 0x83,
];

/// Fixed-width 2048-bit unsigned integer (little-endian u64 limbs) for 1024-bit RSA (e = 65537).
#[derive(Clone, Copy, PartialEq, Eq)]
struct U2048([u64; 32]);

impl U2048 {
    fn from_be_bytes_128(bytes: &[u8]) -> Self {
        let mut padded = [0u8; 128];
        let len = bytes.len().min(128);
        padded[128 - len..].copy_from_slice(&bytes[bytes.len() - len..]);
        let mut limbs = [0u64; 32];
        for i in 0..16 {
            let off = 128 - (i + 1) * 8;
            limbs[i] = u64::from_be_bytes(padded[off..off + 8].try_into().unwrap());
        }
        Self(limbs)
    }

    fn to_be_bytes_128(&self) -> [u8; 128] {
        let mut out = [0u8; 128];
        for i in 0..16 {
            let off = 128 - (i + 1) * 8;
            out[off..off + 8].copy_from_slice(&self.0[i].to_be_bytes());
        }
        out
    }

    fn ge(&self, other: &Self) -> bool {
        for i in (0..32).rev() {
            if self.0[i] != other.0[i] {
                return self.0[i] > other.0[i];
            }
        }
        true
    }

    fn sub_assign(&mut self, other: &Self) {
        let mut borrow = 0u128;
        for i in 0..32 {
            let a = self.0[i] as u128;
            let b = other.0[i] as u128 + borrow;
            if a >= b {
                self.0[i] = (a - b) as u64;
                borrow = 0;
            } else {
                self.0[i] = ((1u128 << 64) + a - b) as u64;
                borrow = 1;
            }
        }
    }

    fn shl1(&mut self) {
        let mut carry = 0u64;
        for i in 0..32 {
            let next = self.0[i] >> 63;
            self.0[i] = (self.0[i] << 1) | carry;
            carry = next;
        }
    }

    fn shr1(&mut self) {
        let mut carry = 0u64;
        for i in (0..32).rev() {
            let next = self.0[i] << 63;
            self.0[i] = (self.0[i] >> 1) | carry;
            carry = next;
        }
    }

    fn bit_len(&self) -> usize {
        for i in (0..32).rev() {
            if self.0[i] != 0 {
                return i * 64 + (64 - self.0[i].leading_zeros() as usize);
            }
        }
        0
    }

    fn mul_mod_1024(a: &Self, b: &Self, m: &Self) -> Self {
        let mut prod = [0u64; 32];
        for i in 0..16 {
            let mut carry = 0u128;
            for j in 0..16 {
                let cur = prod[i + j] as u128 + (a.0[i] as u128) * (b.0[j] as u128) + carry;
                prod[i + j] = cur as u64;
                carry = cur >> 64;
            }
            prod[i + 16] = prod[i + 16].wrapping_add(carry as u64);
        }
        let mut rem = Self(prod);
        let m_bits = m.bit_len();
        let r_bits = rem.bit_len();
        if r_bits < m_bits {
            return rem;
        }
        let shift = r_bits - m_bits;
        let mut divisor = *m;
        for _ in 0..shift {
            divisor.shl1();
        }
        for _ in 0..=shift {
            if rem.ge(&divisor) {
                rem.sub_assign(&divisor);
            }
            divisor.shr1();
        }
        rem
    }
}

fn rsa_pow_65537(msg_128: &[u8]) -> [u8; 128] {
    let m = U2048::from_be_bytes_128(&RSA_MODULUS_BE);
    let base = U2048::from_be_bytes_128(msg_128);
    let mut acc = base;
    for _ in 0..16 {
        acc = U2048::mul_mod_1024(&acc, &acc, &m);
    }
    acc = U2048::mul_mod_1024(&acc, &base, &m);
    acc.to_be_bytes_128()
}

fn rsa_gen_key(rand_key: &[u8], sk_len: usize) -> Vec<u8> {
    let mut xor_key = vec![0u8; sk_len];
    let mut length = sk_len * (sk_len.saturating_sub(1));
    let mut index = 0usize;
    for i in 0..sk_len {
        let x = rand_key[i].wrapping_add(G_KTS[index]);
        xor_key[i] = G_KTS[length] ^ x;
        length = length.saturating_sub(sk_len);
        index += sk_len;
    }
    xor_key
}

/// Exact match for `p115cipher.util.xor` (handles `len(src) & 3` prefix slice before stepping by `key.len()`).
fn p115_xor(src: &[u8], key: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    let rem = src.len() & 0b11;
    for idx in 0..rem {
        out.push(src[idx] ^ key[idx]);
    }
    let mut pos = rem;
    while pos < src.len() {
        let chunk = (src.len() - pos).min(key.len());
        for k in 0..chunk {
            out.push(src[pos + k] ^ key[k]);
        }
        pos += chunk;
    }
    out
}

pub fn m115_rsa_encrypt(plaintext: &[u8]) -> String {
    let rand_key = [0u8; 16];
    let mut tmp = p115_xor(plaintext, RSA_KEY);
    tmp.reverse();
    let mut xor_data = Vec::with_capacity(16 + tmp.len());
    xor_data.extend_from_slice(&rand_key);
    xor_data.extend_from_slice(&p115_xor(&tmp, G_KEY_L));

    let mut cipher_bytes = Vec::new();
    for chunk in xor_data.chunks(117) {
        let mut block = Vec::with_capacity(128);
        block.push(0x00);
        block.resize(128 - 1 - chunk.len(), 0x02);
        block.push(0x00);
        block.extend_from_slice(chunk);
        cipher_bytes.extend_from_slice(&rsa_pow_65537(&block));
    }
    B64.encode(cipher_bytes)
}

pub fn m115_rsa_decrypt(cipher_b64: &str) -> Result<Vec<u8>> {
    let raw = B64.decode(cipher_b64.trim())?;
    ensure!(!raw.is_empty() && raw.len() % 128 == 0, "m115 密文长度异常");
    let mut decoded = Vec::new();
    for block in raw.chunks(128) {
        let p_be = rsa_pow_65537(block);
        let first_nz = p_be.iter().position(|&b| b != 0).unwrap_or(p_be.len());
        let stripped = &p_be[first_nz..];
        if let Some(sep) = stripped.iter().position(|&b| b == 0) {
            decoded.extend_from_slice(&stripped[sep + 1..]);
        } else {
            bail!("m115 PKCS1 解包失败");
        }
    }
    ensure!(decoded.len() >= 16, "m115 明文头不足 16 字节");
    let rand_key = &decoded[..16];
    let key_l = rsa_gen_key(rand_key, 12);
    let mut tmp = p115_xor(&decoded[16..], &key_l);
    tmp.reverse();
    Ok(p115_xor(&tmp, RSA_KEY))
}

fn crc32_ieee(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

pub fn ecdh_encode_token(timestamp: u32) -> String {
    let mut token = Vec::with_capacity(49);
    token.extend_from_slice(&AES_PUBKEY[..15]);
    token.extend_from_slice(b"\x00s\x00\x00\x00");
    token.extend_from_slice(&timestamp.to_le_bytes());
    token.extend_from_slice(&AES_PUBKEY[15..]);
    token.extend_from_slice(b"\x00\x01\x00\x00\x00");

    let mut crc_input = Vec::with_capacity(CRC_SALT.len() + token.len());
    crc_input.extend_from_slice(CRC_SALT);
    crc_input.extend_from_slice(&token);
    let crc = crc32_ieee(&crc_input);
    token.extend_from_slice(&crc.to_le_bytes());
    B64.encode(token)
}

pub fn ecdh_aes_encrypt(plaintext: &[u8]) -> Vec<u8> {
    let pad_len = (16 - (plaintext.len() % 16)) % 16;
    let mut padded = plaintext.to_vec();
    if pad_len > 0 {
        padded.extend(std::iter::repeat_n(pad_len as u8, pad_len));
    }
    let cipher = Aes128::new(GenericArray::from_slice(&AES_KEY));
    let mut prev = AES_IV;
    let mut out = Vec::with_capacity(padded.len());
    for chunk in padded.chunks_exact(16) {
        let mut block = [0u8; 16];
        for i in 0..16 {
            block[i] = chunk[i] ^ prev[i];
        }
        let ga = GenericArray::from_mut_slice(&mut block);
        cipher.encrypt_block(ga);
        prev = block;
        out.extend_from_slice(&block);
    }
    out
}

fn lz4_block_decompress(src: &[u8]) -> Result<Vec<u8>> {
    let mut ptr = 0usize;
    let mut dest = Vec::with_capacity(src.len() * 2 + 64);
    while ptr < src.len() {
        let token = src[ptr];
        ptr += 1;
        let mut lit_len = (token >> 4) as usize;
        if lit_len == 15 {
            while ptr < src.len() && src[ptr] == 255 {
                lit_len += 255;
                ptr += 1;
            }
            if ptr < src.len() {
                lit_len += src[ptr] as usize;
                ptr += 1;
            }
        }
        if lit_len > 0 {
            let end = (ptr + lit_len).min(src.len());
            dest.extend_from_slice(&src[ptr..end]);
            ptr = end;
        }
        if ptr >= src.len() {
            break;
        }
        ensure!(ptr + 2 <= src.len(), "LZ4 offset 截断");
        let offset = (src[ptr] as usize) | ((src[ptr + 1] as usize) << 8);
        ptr += 2;
        ensure!(offset > 0 && offset <= dest.len(), "LZ4 offset 越界");
        let mut match_len = ((token & 0x0F) as usize) + 4;
        if (token & 0x0F) == 15 {
            while ptr < src.len() && src[ptr] == 255 {
                match_len += 255;
                ptr += 1;
            }
            if ptr < src.len() {
                match_len += src[ptr] as usize;
                ptr += 1;
            }
        }
        let start_pos = dest.len() - offset;
        for i in 0..match_len {
            let b = dest[start_pos + i];
            dest.push(b);
        }
    }
    Ok(dest)
}

pub fn ecdh_aes_decrypt(cipher_data: &[u8]) -> Result<Vec<u8>> {
    let aligned_len = cipher_data.len() & !15;
    ensure!(aligned_len >= 16, "ECDH AES 响应不足一个分组");
    let cipher = Aes128::new(GenericArray::from_slice(&AES_KEY));
    let mut prev = AES_IV;
    let mut plain = Vec::with_capacity(aligned_len);
    for chunk in cipher_data[..aligned_len].chunks_exact(16) {
        let mut block: [u8; 16] = chunk.try_into().unwrap();
        let ga = GenericArray::from_mut_slice(&mut block);
        cipher.decrypt_block(ga);
        for i in 0..16 {
            plain.push(block[i] ^ prev[i]);
        }
        prev.copy_from_slice(chunk);
    }
    if let Some(&last) = plain.last() {
        let pad = last as usize;
        if (1..16).contains(&pad) && plain.len() >= pad && plain[plain.len() - pad..].iter().all(|&b| b == last) {
            plain.truncate(plain.len() - pad);
        }
    }
    let mut decompressed = Vec::new();
    let mut slice = plain.as_slice();
    while slice.len() > 2 {
        let chunk_len = (slice[0] as usize) | ((slice[1] as usize) << 8);
        if chunk_len == 0 || slice.len() < 2 + chunk_len {
            break;
        }
        decompressed.extend_from_slice(&lz4_block_decompress(&slice[2..2 + chunk_len])?);
        slice = &slice[2 + chunk_len..];
    }
    Ok(decompressed)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

pub fn hex_upper(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{b:02X}");
    }
    s
}

/// Builds `(k_ec_query_param, encrypted_body_bytes)` for `https://uplb.115.com/4.0/initupload.php`.
pub fn make_upload_init_request(
    userid: &str,
    userkey: &str,
    file_sha1_upper: &str,
    filename: &str,
    filesize: u64,
    target: &str,
    sign_key: &str,
    sign_val: &str,
    timestamp: u32,
) -> (String, Vec<u8>) {
    let appversion = "36.0.0";
    let mut inner_sha = Sha1::new();
    inner_sha.update(format!("{userid}{file_sha1_upper}{target}0").as_bytes());
    let inner_hex = hex_lower(&inner_sha.finalize());

    let mut outer_sha = Sha1::new();
    outer_sha.update(userkey.as_bytes());
    outer_sha.update(inner_hex.as_bytes());
    outer_sha.update(b"000000");
    let sig = hex_upper(&outer_sha.finalize());

    let uid_num: u64 = userid.trim().parse().unwrap_or(0);
    let uid_md5_hex = hex_lower(md5::compute(format!("{uid_num}").as_bytes()).as_slice());

    let mut token_ctx = md5::Context::new();
    token_ctx.consume(MD5_SALT);
    token_ctx.consume(format!("{file_sha1_upper}{filesize}{sign_key}{sign_val}{userid}{timestamp}").as_bytes());
    token_ctx.consume(uid_md5_hex.as_bytes());
    token_ctx.consume(appversion.as_bytes());
    let token = hex_lower(token_ctx.compute().as_slice());

    let mut pairs: Vec<(&str, String)> = vec![
        ("appversion", appversion.to_string()),
        ("fileid", file_sha1_upper.to_string()),
        ("filename", filename.to_string()),
        ("filesize", filesize.to_string()),
        ("sig", sig),
        ("t", timestamp.to_string()),
        ("target", target.to_string()),
        ("token", token),
        ("topupload", "true".to_string()),
        ("userid", userid.to_string()),
        ("userkey", userkey.to_string()),
    ];
    if !sign_key.is_empty() {
        pairs.push(("sign_key", sign_key.to_string()));
    }
    if !sign_val.is_empty() {
        pairs.push(("sign_val", sign_val.to_string()));
    }
    pairs.sort_by(|a, b| a.0.cmp(b.0));

    let mut ser = form_urlencoded::Serializer::new(String::new());
    for (k, v) in &pairs {
        if !v.is_empty() {
            ser.append_pair(k, v);
        }
    }
    let encoded_form = ser.finish();
    let k_ec = ecdh_encode_token(timestamp);
    let body = ecdh_aes_encrypt(encoded_form.as_bytes());
    (k_ec, body)
}

const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

fn transtab_src(prefix: &str) -> Option<&'static [u8; 36]> {
    match prefix {
        "a" => Some(b"fuln1ytpj3smg8d5a094qh7cxkbi62zvewro"),
        "b" => Some(b"sk721n9a0emlfpcrzbqdw3gjh6ty5xui48vo"),
        "c" => Some(b"ywcz3hite6f1j0guoakvdb2ns7p8qr9ml5x4"),
        "d" => Some(b"rq2vl5o7wsken9u8tp4jg3zbyc6xmhifd01a"),
        "e" => Some(b"ljm9eqbcfhw7ktv3x1dgp5ua8y6s4znr2io0"),
        "fa" => Some(b"fumk0ytpj3sng8d5a194qh7cxlbi62zvewro"),
        "fb" => Some(b"sk732o9a1enmfpcrzbqdw4gjh6ty5xui08vl"),
        "fc" => Some(b"ywcz6hite9f4j3gup2kvdb5osal0qr1nm8x7"),
        "fd" => Some(b"on6vl0r2wpkeq9u3ts8jg7zbyc1xmhifd45a"),
        "fe" => Some(b"ljm0es2cfhwakqv6x4dgp8r1by9u7znt5io3"),
        _ => None,
    }
}

/// Converts a 115 `pickcode` (e.g. `"ay4u93dndfz6ehj39"`) to its numeric 115 `file_id` offline.
pub fn pickcode_to_id(pickcode: &str) -> Option<u64> {
    let pc = pickcode.trim().to_ascii_lowercase();
    if pc.is_empty() {
        return None;
    }
    if pc.bytes().all(|b| b.is_ascii_digit()) {
        return pc.parse::<u64>().ok();
    }
    let (prefix, cipher) = if pc.starts_with('f') {
        if pc.len() < 7 {
            return None;
        }
        (&pc[..2], &pc[2..pc.len() - 4])
    } else {
        if pc.len() < 6 {
            return None;
        }
        (&pc[..1], &pc[1..pc.len() - 4])
    };
    let tab = transtab_src(prefix)?;
    let mut plain_b36 = String::with_capacity(cipher.len());
    for ch in cipher.bytes() {
        let idx = ALPHABET.iter().position(|&a| a == ch)?;
        plain_b36.push(tab[idx] as char);
    }
    u64::from_str_radix(&plain_b36, 36).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pickcode_to_id_decoding() {
        // Real sample pickcode from /vol3/1000/strm: ay4u93dndfz6ehj39
        let id = pickcode_to_id("ay4u93dndfz6ehj39").expect("valid pickcode");
        assert!(id > 1_000_000_000_000);
        // Numeric passthrough
        assert_eq!(pickcode_to_id("3560963432360445692"), Some(3560963432360445692));
        assert_eq!(pickcode_to_id("invalid!"), None);
    }

    #[test]
    fn test_m115_rsa_exact_python_vectors() {
        // Verified bit-for-bit against Python `p115cipher.rsa_encrypt` (both aligned and odd lengths)
        assert_eq!(
            m115_rsa_encrypt(br#"{"pickcode":"ay4u93dndfz6ehj39"}"#),
            "GdOSwe/mENL4s/sLWYPobbYP+yiOZStOeSLuDTmI6zEhe89wLTAKQt5gNlO1eKxlBHDS10dzv/yxUiP40AJ1wA4bT9rVva31OyadVximXf/XsT+DPci7YlB/p0r6SuH9VqrhvfymAplfRyfcdWlvRM18BIM3Ghdx1UFLSFpWl+o="
        );
        assert_eq!(
            m115_rsa_encrypt(br#"{"a":1}"#),
            "PqTY9OdYSSUVtQAM+x6VBMJXw7LxeorxLRF81hJrXGW7yWvX1Q7ugYL5QouEHWc3SfUy1p+QOiNd8nOmuWvW2VRnUumR6KpjuOy9mP7q8AmuzEysYLmgJcWT4puUEHTv1RfGvc1KFEAmNTPSWGwwVetv2/y37ciBHsxvbA2RlyU="
        );
    }

    #[test]
    fn test_ecdh_token_and_upload_payload() {
        let (k_ec, body) = make_upload_init_request(
            "80000895",
            "deadbeef",
            "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709",
            "test.mkv",
            1048576,
            "U_1_0",
            "",
            "",
            1_700_000_000,
        );
        assert!(!k_ec.is_empty());
        assert_eq!(body.len() % 16, 0);
    }
}
