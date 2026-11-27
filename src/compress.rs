use anyhow::{Result, bail};
use std::{
    ffi::{CStr, c_char, c_int, c_uint, c_ulong, c_void},
    io::{self, Read},
    ptr,
};
#[cfg(test)]
use std::io::Write;

// --- libz (zlib) FFI ---

#[repr(C)]
struct ZStream {
    next_in: *const u8,
    avail_in: c_uint,
    total_in: c_ulong,
    next_out: *mut u8,
    avail_out: c_uint,
    total_out: c_ulong,
    msg: *const c_char,
    state: *mut c_void,
    zalloc: *mut c_void,
    zfree: *mut c_void,
    opaque: *mut c_void,
    data_type: c_int,
    adler: c_ulong,
    reserved: c_ulong,
}

const Z_NO_FLUSH: c_int = 0;
#[cfg(test)]
const Z_FINISH: c_int = 4;
const Z_OK: c_int = 0;
const Z_STREAM_END: c_int = 1;
const Z_BUF_ERROR: c_int = -5;
#[cfg(test)]
const Z_DEFLATED: c_int = 8;
#[cfg(test)]
const Z_DEFAULT_STRATEGY: c_int = 0;

#[link(name = "z")]
unsafe extern "C" {
    fn zlibVersion() -> *const c_char;
    fn inflateInit2_(strm: *mut ZStream, window_bits: c_int, version: *const c_char, stream_size: c_int) -> c_int;
    fn inflate(strm: *mut ZStream, flush: c_int) -> c_int;
    fn inflateReset(strm: *mut ZStream) -> c_int;
    fn inflateEnd(strm: *mut ZStream) -> c_int;
    #[cfg(test)]
    fn deflateInit2_(
        strm: *mut ZStream,
        level: c_int,
        method: c_int,
        window_bits: c_int,
        mem_level: c_int,
        strategy: c_int,
        version: *const c_char,
        stream_size: c_int,
    ) -> c_int;
    #[cfg(test)]
    fn deflate(strm: *mut ZStream, flush: c_int) -> c_int;
    #[cfg(test)]
    fn deflateEnd(strm: *mut ZStream) -> c_int;
}

fn zero_zstream() -> ZStream {
    ZStream {
        next_in: ptr::null(),
        avail_in: 0,
        total_in: 0,
        next_out: ptr::null_mut(),
        avail_out: 0,
        total_out: 0,
        msg: ptr::null(),
        state: ptr::null_mut(),
        zalloc: ptr::null_mut(),
        zfree: ptr::null_mut(),
        opaque: ptr::null_mut(),
        data_type: 0,
        adler: 0,
        reserved: 0,
    }
}

pub struct GzDecoder<'a> {
    strm: Box<ZStream>,
    input: &'a [u8],
    pos: usize,
    done: bool,
}

impl<'a> GzDecoder<'a> {
    pub fn new(input: &'a [u8]) -> Result<Self> {
        let mut strm = Box::new(zero_zstream());
        let rc = unsafe {
            inflateInit2_(
                &mut *strm,
                15 + 16,
                zlibVersion(),
                std::mem::size_of::<ZStream>() as c_int,
            )
        };
        if rc != Z_OK {
            bail!("zlib inflateInit2 failed: {rc}");
        }
        Ok(Self {
            strm,
            input,
            pos: 0,
            done: false,
        })
    }
}

impl Drop for GzDecoder<'_> {
    fn drop(&mut self) {
        unsafe {
            inflateEnd(&mut *self.strm);
        }
    }
}

impl Read for GzDecoder<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        loop {
            let remaining = &self.input[self.pos..];
            let chunk = remaining.len().min(u32::MAX as usize);
            self.strm.next_in = remaining.as_ptr();
            self.strm.avail_in = chunk as c_uint;
            let out_cap = buf.len().min(u32::MAX as usize);
            self.strm.next_out = buf.as_mut_ptr();
            self.strm.avail_out = out_cap as c_uint;

            let rc = unsafe { inflate(&mut *self.strm, Z_NO_FLUSH) };
            let consumed = chunk - self.strm.avail_in as usize;
            let produced = out_cap - self.strm.avail_out as usize;
            self.pos += consumed;

            match rc {
                Z_OK => {
                    if produced > 0 {
                        return Ok(produced);
                    }
                    if consumed == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "truncated gzip stream",
                        ));
                    }
                }
                Z_STREAM_END => {
                    // Handle concatenated multi-member gzip streams.
                    if self.pos < self.input.len() && self.input[self.pos..].starts_with(&[0x1f, 0x8b]) {
                        let reset_rc = unsafe { inflateReset(&mut *self.strm) };
                        if reset_rc != Z_OK {
                            return Err(io::Error::other(format!("zlib inflateReset failed: {reset_rc}")));
                        }
                    } else {
                        self.done = true;
                    }
                    if produced > 0 || self.done {
                        return Ok(produced);
                    }
                }
                Z_BUF_ERROR if produced > 0 => return Ok(produced),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("zlib inflate failed: {rc}"),
                    ));
                }
            }
        }
    }
}

pub fn gzip_decompress(input: &[u8], hint_capacity: usize) -> Result<Vec<u8>> {
    let mut decoder = GzDecoder::new(input)?;
    let mut out = Vec::with_capacity(hint_capacity.min(64 * 1024 * 1024));
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(test)]
pub struct GzEncoder<W: Write> {
    strm: Box<ZStream>,
    writer: Option<W>,
    buf: Vec<u8>,
}

#[cfg(test)]
impl<W: Write> GzEncoder<W> {
    pub fn new(writer: W, level: i32) -> Self {
        let mut strm = Box::new(zero_zstream());
        let rc = unsafe {
            deflateInit2_(
                &mut *strm,
                level as c_int,
                Z_DEFLATED,
                15 + 16,
                8,
                Z_DEFAULT_STRATEGY,
                zlibVersion(),
                std::mem::size_of::<ZStream>() as c_int,
            )
        };
        assert_eq!(rc, Z_OK, "zlib deflateInit2 failed: {rc}");
        Self {
            strm,
            writer: Some(writer),
            buf: vec![0u8; 16384],
        }
    }

    fn pump(&mut self, data: &[u8], flush: c_int) -> io::Result<()> {
        let mut offset = 0;
        loop {
            let chunk = (data.len() - offset).min(u32::MAX as usize);
            self.strm.next_in = data[offset..].as_ptr();
            self.strm.avail_in = chunk as c_uint;
            self.strm.next_out = self.buf.as_mut_ptr();
            self.strm.avail_out = self.buf.len() as c_uint;
            let rc = unsafe { deflate(&mut *self.strm, flush) };
            let consumed = chunk - self.strm.avail_in as usize;
            let produced = self.buf.len() - self.strm.avail_out as usize;
            offset += consumed;
            if produced > 0 {
                self.writer.as_mut().unwrap().write_all(&self.buf[..produced])?;
            }
            match rc {
                Z_STREAM_END => return Ok(()),
                Z_OK | Z_BUF_ERROR => {
                    if flush == Z_NO_FLUSH && offset == data.len() && produced < self.buf.len() {
                        return Ok(());
                    }
                    if consumed == 0 && produced == 0 && rc == Z_BUF_ERROR {
                        return Ok(());
                    }
                }
                _ => return Err(io::Error::other(format!("zlib deflate failed: {rc}"))),
            }
        }
    }

    pub fn finish(mut self) -> io::Result<W> {
        self.pump(&[], Z_FINISH)?;
        let mut w = self.writer.take().unwrap();
        w.flush()?;
        Ok(w)
    }
}

#[cfg(test)]
impl<W: Write> Write for GzEncoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pump(buf, Z_NO_FLUSH)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.as_mut().unwrap().flush()
    }
}

#[cfg(test)]
impl<W: Write> Drop for GzEncoder<W> {
    fn drop(&mut self) {
        unsafe {
            deflateEnd(&mut *self.strm);
        }
    }
}

// --- libzstd FFI ---

#[repr(C)]
struct ZstdInBuffer {
    src: *const c_void,
    size: usize,
    pos: usize,
}

#[repr(C)]
struct ZstdOutBuffer {
    dst: *mut c_void,
    size: usize,
    pos: usize,
}

const ZSTD_CONTENTSIZE_UNKNOWN: u64 = u64::MAX;
const ZSTD_CONTENTSIZE_ERROR: u64 = u64::MAX - 1;

#[link(name = "zstd")]
unsafe extern "C" {
    fn ZSTD_compress(
        dst: *mut c_void,
        dst_capacity: usize,
        src: *const c_void,
        src_size: usize,
        compression_level: c_int,
    ) -> usize;
    fn ZSTD_decompress(
        dst: *mut c_void,
        dst_capacity: usize,
        src: *const c_void,
        compressed_size: usize,
    ) -> usize;
    fn ZSTD_getFrameContentSize(src: *const c_void, src_size: usize) -> u64;
    fn ZSTD_compressBound(src_size: usize) -> usize;
    fn ZSTD_isError(code: usize) -> c_uint;
    fn ZSTD_getErrorName(code: usize) -> *const c_char;
    fn ZSTD_createDStream() -> *mut c_void;
    fn ZSTD_freeDStream(zds: *mut c_void) -> usize;
    fn ZSTD_initDStream(zds: *mut c_void) -> usize;
    fn ZSTD_decompressStream(
        zds: *mut c_void,
        output: *mut ZstdOutBuffer,
        input: *mut ZstdInBuffer,
    ) -> usize;
    #[cfg(test)]
    fn ZSTD_createCStream() -> *mut c_void;
    #[cfg(test)]
    fn ZSTD_freeCStream(zcs: *mut c_void) -> usize;
    #[cfg(test)]
    fn ZSTD_initCStream(zcs: *mut c_void, compression_level: c_int) -> usize;
    #[cfg(test)]
    fn ZSTD_compressStream(
        zcs: *mut c_void,
        output: *mut ZstdOutBuffer,
        input: *mut ZstdInBuffer,
    ) -> usize;
    #[cfg(test)]
    fn ZSTD_endStream(zcs: *mut c_void, output: *mut ZstdOutBuffer) -> usize;
}

fn zstd_err(code: usize) -> String {
    unsafe {
        CStr::from_ptr(ZSTD_getErrorName(code))
            .to_string_lossy()
            .into_owned()
    }
}

pub fn zstd_compress(input: &[u8], level: i32) -> Result<Vec<u8>> {
    let bound = unsafe { ZSTD_compressBound(input.len()) };
    if unsafe { ZSTD_isError(bound) } != 0 {
        bail!("zstd compressBound failed: {}", zstd_err(bound));
    }
    let mut out = vec![0u8; bound];
    let n = unsafe {
        ZSTD_compress(
            out.as_mut_ptr().cast(),
            out.len(),
            input.as_ptr().cast(),
            input.len(),
            level as c_int,
        )
    };
    if unsafe { ZSTD_isError(n) } != 0 {
        bail!("zstd compress failed: {}", zstd_err(n));
    }
    out.truncate(n);
    Ok(out)
}

pub fn zstd_frame_content_size(input: &[u8]) -> Result<Option<u64>> {
    let size = unsafe { ZSTD_getFrameContentSize(input.as_ptr().cast(), input.len()) };
    match size {
        ZSTD_CONTENTSIZE_ERROR => bail!("invalid zstd frame"),
        ZSTD_CONTENTSIZE_UNKNOWN => Ok(None),
        n => Ok(Some(n)),
    }
}

pub fn zstd_decompress(input: &[u8], capacity: usize) -> Result<Vec<u8>> {
    let mut out = vec![0u8; capacity];
    let n = unsafe {
        ZSTD_decompress(
            out.as_mut_ptr().cast(),
            out.len(),
            input.as_ptr().cast(),
            input.len(),
        )
    };
    if unsafe { ZSTD_isError(n) } != 0 {
        bail!("zstd decompress failed: {}", zstd_err(n));
    }
    out.truncate(n);
    Ok(out)
}

pub struct ZstdDecoder<'a> {
    dstream: *mut c_void,
    input: &'a [u8],
    pos: usize,
    done: bool,
}

impl<'a> ZstdDecoder<'a> {
    pub fn new(input: &'a [u8]) -> Result<Self> {
        let dstream = unsafe { ZSTD_createDStream() };
        if dstream.is_null() {
            bail!("zstd createDStream failed");
        }
        let rc = unsafe { ZSTD_initDStream(dstream) };
        if unsafe { ZSTD_isError(rc) } != 0 {
            unsafe { ZSTD_freeDStream(dstream) };
            bail!("zstd initDStream failed: {}", zstd_err(rc));
        }
        Ok(Self {
            dstream,
            input,
            pos: 0,
            done: false,
        })
    }
}

impl Drop for ZstdDecoder<'_> {
    fn drop(&mut self) {
        unsafe {
            ZSTD_freeDStream(self.dstream);
        }
    }
}

impl Read for ZstdDecoder<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        loop {
            let mut in_buf = ZstdInBuffer {
                src: self.input.as_ptr().cast(),
                size: self.input.len(),
                pos: self.pos,
            };
            let mut out_buf = ZstdOutBuffer {
                dst: buf.as_mut_ptr().cast(),
                size: buf.len(),
                pos: 0,
            };
            let rc = unsafe { ZSTD_decompressStream(self.dstream, &mut out_buf, &mut in_buf) };
            if unsafe { ZSTD_isError(rc) } != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("zstd decompressStream failed: {}", zstd_err(rc)),
                ));
            }
            let progressed = in_buf.pos > self.pos || out_buf.pos > 0;
            self.pos = in_buf.pos;
            if rc == 0 && self.pos >= self.input.len() {
                self.done = true;
            }
            if out_buf.pos > 0 {
                return Ok(out_buf.pos);
            }
            if self.done {
                return Ok(0);
            }
            if !progressed {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated zstd stream",
                ));
            }
        }
    }
}

#[cfg(test)]
pub fn zstd_compress_stream(input: &[u8], level: i32) -> Result<Vec<u8>> {
    let cstream = unsafe { ZSTD_createCStream() };
    if cstream.is_null() {
        bail!("zstd createCStream failed");
    }
    let init = unsafe { ZSTD_initCStream(cstream, level as c_int) };
    if unsafe { ZSTD_isError(init) } != 0 {
        unsafe { ZSTD_freeCStream(cstream) };
        bail!("zstd initCStream failed: {}", zstd_err(init));
    }
    let mut out = Vec::new();
    let mut scratch = vec![0u8; 16384];
    let mut in_buf = ZstdInBuffer {
        src: input.as_ptr().cast(),
        size: input.len(),
        pos: 0,
    };
    while in_buf.pos < in_buf.size {
        let mut out_buf = ZstdOutBuffer {
            dst: scratch.as_mut_ptr().cast(),
            size: scratch.len(),
            pos: 0,
        };
        let rc = unsafe { ZSTD_compressStream(cstream, &mut out_buf, &mut in_buf) };
        if unsafe { ZSTD_isError(rc) } != 0 {
            unsafe { ZSTD_freeCStream(cstream) };
            bail!("zstd compressStream failed: {}", zstd_err(rc));
        }
        out.extend_from_slice(&scratch[..out_buf.pos]);
    }
    loop {
        let mut out_buf = ZstdOutBuffer {
            dst: scratch.as_mut_ptr().cast(),
            size: scratch.len(),
            pos: 0,
        };
        let rc = unsafe { ZSTD_endStream(cstream, &mut out_buf) };
        if unsafe { ZSTD_isError(rc) } != 0 {
            unsafe { ZSTD_freeCStream(cstream) };
            bail!("zstd endStream failed: {}", zstd_err(rc));
        }
        out.extend_from_slice(&scratch[..out_buf.pos]);
        if rc == 0 {
            break;
        }
    }
    unsafe { ZSTD_freeCStream(cstream) };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_roundtrip_and_multi_member() {
        let payload = b"hello drvjs gzip test payload repeated ".repeat(200);
        let mut enc = GzEncoder::new(Vec::new(), 1);
        enc.write_all(&payload).unwrap();
        let gz = enc.finish().unwrap();
        assert_eq!(gzip_decompress(&gz, payload.len()).unwrap(), payload);

        let mut two = gz.clone();
        two.extend_from_slice(&gz);
        let mut expected = payload.clone();
        expected.extend_from_slice(&payload);
        assert_eq!(gzip_decompress(&two, 64).unwrap(), expected);
    }

    #[test]
    fn zstd_roundtrip_bulk_and_streaming() {
        let payload = b"hello drvjs zstd test payload repeated ".repeat(200);
        let bulk = zstd_compress(&payload, 1).unwrap();
        assert_eq!(zstd_frame_content_size(&bulk).unwrap(), Some(payload.len() as u64));
        assert_eq!(zstd_decompress(&bulk, payload.len()).unwrap(), payload);

        let streamed = zstd_compress_stream(&payload, 1).unwrap();
        assert_eq!(zstd_frame_content_size(&streamed).unwrap(), None);
        let mut dec = ZstdDecoder::new(&streamed).unwrap();
        let mut out = Vec::new();
        dec.read_to_end(&mut out).unwrap();
        assert_eq!(out, payload);
    }
}
