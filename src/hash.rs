use anyhow::{Result, bail};
use std::{
    ffi::{c_int, c_uint, c_void},
    ptr,
};

// --- Base64 (RFC 4648 standard alphabet) ---

const B64_ENC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut chunks = input.chunks_exact(3);
    for c in chunks.by_ref() {
        let n = ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | (c[2] as u32);
        out.push(B64_ENC[((n >> 18) & 63) as usize] as char);
        out.push(B64_ENC[((n >> 12) & 63) as usize] as char);
        out.push(B64_ENC[((n >> 6) & 63) as usize] as char);
        out.push(B64_ENC[(n & 63) as usize] as char);
    }
    let rem = chunks.remainder();
    if rem.len() == 1 {
        let n = (rem[0] as u32) << 16;
        out.push(B64_ENC[((n >> 18) & 63) as usize] as char);
        out.push(B64_ENC[((n >> 12) & 63) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem.len() == 2 {
        let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
        out.push(B64_ENC[((n >> 18) & 63) as usize] as char);
        out.push(B64_ENC[((n >> 12) & 63) as usize] as char);
        out.push(B64_ENC[((n >> 6) & 63) as usize] as char);
        out.push('=');
    }
    out
}

pub fn base64_decode(input: &str) -> Result<Vec<u8>> {
    let bytes = input.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        bail!("invalid base64 length");
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (ci, chunk) in bytes.chunks_exact(4).enumerate() {
        let is_last = (ci + 1) * 4 == bytes.len();
        let mut vals = [0u8; 4];
        let mut pad = 0usize;
        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                if !is_last || i < 2 || (i == 2 && chunk[3] != b'=') {
                    bail!("invalid base64 padding");
                }
                pad += 1;
            } else {
                if pad > 0 {
                    bail!("invalid base64 padding");
                }
                vals[i] = match b {
                    b'A'..=b'Z' => b - b'A',
                    b'a'..=b'z' => b - b'a' + 26,
                    b'0'..=b'9' => b - b'0' + 52,
                    b'+' => 62,
                    b'/' => 63,
                    _ => bail!("invalid base64 character"),
                };
            }
        }
        let n = ((vals[0] as u32) << 18)
            | ((vals[1] as u32) << 12)
            | ((vals[2] as u32) << 6)
            | (vals[3] as u32);
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad == 0 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

// --- Hardware-accelerated SHA-256/384/512 via libcrypto (OpenSSL EVP) ---

#[link(name = "crypto")]
unsafe extern "C" {
    fn EVP_MD_CTX_new() -> *mut c_void;
    fn EVP_MD_CTX_free(ctx: *mut c_void);
    fn EVP_DigestInit_ex(ctx: *mut c_void, md: *const c_void, engine: *mut c_void) -> c_int;
    fn EVP_DigestUpdate(ctx: *mut c_void, d: *const c_void, cnt: usize) -> c_int;
    fn EVP_DigestFinal_ex(ctx: *mut c_void, md: *mut u8, s: *mut c_uint) -> c_int;
    fn EVP_Digest(
        data: *const c_void,
        count: usize,
        md: *mut u8,
        size: *mut c_uint,
        md_type: *const c_void,
        engine: *mut c_void,
    ) -> c_int;
    fn EVP_sha256() -> *const c_void;
    fn EVP_sha384() -> *const c_void;
    fn EVP_sha512() -> *const c_void;
}

struct EvpHasher {
    ctx: *mut c_void,
}

impl EvpHasher {
    fn new(md: *const c_void) -> Self {
        let ctx = unsafe { EVP_MD_CTX_new() };
        assert!(!ctx.is_null(), "EVP_MD_CTX_new failed");
        let rc = unsafe { EVP_DigestInit_ex(ctx, md, ptr::null_mut()) };
        assert_eq!(rc, 1, "EVP_DigestInit_ex failed");
        Self { ctx }
    }

    fn update(&mut self, data: &[u8]) {
        let rc = unsafe { EVP_DigestUpdate(self.ctx, data.as_ptr().cast(), data.len()) };
        assert_eq!(rc, 1, "EVP_DigestUpdate failed");
    }

    fn finalize_into<const N: usize>(self) -> [u8; N] {
        let mut out = [0u8; N];
        let mut len: c_uint = 0;
        let rc = unsafe { EVP_DigestFinal_ex(self.ctx, out.as_mut_ptr(), &mut len) };
        assert_eq!(rc, 1, "EVP_DigestFinal_ex failed");
        assert_eq!(len as usize, N);
        out
    }
}

impl Drop for EvpHasher {
    fn drop(&mut self) {
        unsafe { EVP_MD_CTX_free(self.ctx) };
    }
}

fn evp_digest<const N: usize>(md: *const c_void, data: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    let mut len: c_uint = 0;
    let rc = unsafe {
        EVP_Digest(
            data.as_ptr().cast(),
            data.len(),
            out.as_mut_ptr(),
            &mut len,
            md,
            ptr::null_mut(),
        )
    };
    assert_eq!(rc, 1, "EVP_Digest failed");
    assert_eq!(len as usize, N);
    out
}

pub struct Sha256(EvpHasher);
impl Sha256 {
    pub fn new() -> Self {
        Self(EvpHasher::new(unsafe { EVP_sha256() }))
    }
    pub fn digest(data: impl AsRef<[u8]>) -> [u8; 32] {
        evp_digest(unsafe { EVP_sha256() }, data.as_ref())
    }
    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
    pub fn finalize(self) -> [u8; 32] {
        self.0.finalize_into()
    }
}

pub struct Sha384(EvpHasher);
impl Sha384 {
    pub fn new() -> Self {
        Self(EvpHasher::new(unsafe { EVP_sha384() }))
    }
    pub fn digest(data: impl AsRef<[u8]>) -> [u8; 48] {
        evp_digest(unsafe { EVP_sha384() }, data.as_ref())
    }
    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
    pub fn finalize(self) -> [u8; 48] {
        self.0.finalize_into()
    }
}

pub struct Sha512(EvpHasher);
impl Sha512 {
    pub fn new() -> Self {
        Self(EvpHasher::new(unsafe { EVP_sha512() }))
    }
    pub fn digest(data: impl AsRef<[u8]>) -> [u8; 64] {
        evp_digest(unsafe { EVP_sha512() }, data.as_ref())
    }
    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
    pub fn finalize(self) -> [u8; 64] {
        self.0.finalize_into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nist_sha2_vectors() {
        assert_eq!(
            hex_encode(&Sha256::digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex_encode(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex_encode(&Sha256::digest(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex_encode(&Sha384::digest(b"abc")),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        );
        assert_eq!(
            hex_encode(&Sha512::digest(b"")),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
        assert_eq!(
            hex_encode(&Sha512::digest(b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        assert_eq!(
            hex_encode(&Sha512::digest(
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"
            )),
            "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909"
        );
        let mut h = Sha512::new();
        for chunk in b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu".chunks(7) {
            h.update(chunk);
        }
        assert_eq!(
            hex_encode(&h.finalize()),
            "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909"
        );
    }

    #[test]
    fn rfc4648_base64() {
        for (raw, enc) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(raw), enc);
            assert_eq!(base64_decode(enc).unwrap(), raw);
        }
        assert!(base64_decode("abc").is_err());
        assert!(base64_decode("a=bc").is_err());
    }
}
