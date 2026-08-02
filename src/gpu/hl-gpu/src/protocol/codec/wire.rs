//! Hand-rolled little-endian wire primitives (no serde — the `hl-term-core` discipline).
//!
//! Everything on hl's GPU channel is framed with these: a length-prefixed byte stream, with out-of-band
//! handles (shm fd via `SCM_RIGHTS`, IOSurface mach-port, or dma-buf fd) correlated by id at the transport
//! layer. All integers little-endian; `f32` as raw bits. Ported byte-identically from `hl-gpu::wire`.

use crate::protocol::model::error::{GpuError, Result};

/// Append-only little-endian encoder over a `Vec<u8>`.
#[derive(Default)]
pub struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }
    pub fn with_capacity(n: usize) -> Self {
        Self {
            buf: Vec::with_capacity(n),
        }
    }
    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }
    #[inline]
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    #[inline]
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    #[inline]
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    #[inline]
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    #[inline]
    pub fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    #[inline]
    pub fn f32(&mut self, v: f32) {
        self.u32(v.to_bits());
    }
    #[inline]
    pub fn f64(&mut self, v: f64) {
        self.u64(v.to_bits());
    }
    #[inline]
    pub fn bool(&mut self, v: bool) {
        self.u8(v as u8);
    }
    /// Length-prefixed (u32) raw bytes.
    pub fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.buf.extend_from_slice(b);
    }
    /// Length-prefixed (u32) UTF-8 string.
    pub fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    /// Length-prefixed (u32 word count) `u32` slice — used for SPIR-V shader words.
    pub fn words(&mut self, w: &[u32]) {
        self.u32(w.len() as u32);
        for &x in w {
            self.u32(x);
        }
    }
    /// A length-prefixed sub-frame: run `f` into a scratch encoder and splice it in with a u32 length.
    /// Lets a decoder skip an unknown/variable body without parsing it (forward-compat + framing).
    pub fn frame<F: FnOnce(&mut Encoder)>(&mut self, f: F) {
        let mut inner = Encoder::new();
        f(&mut inner);
        self.bytes(&inner.buf);
    }
}

/// Cursor decoder over a borrowed byte slice. Every read is bounds-checked → `Err(ShortBuffer)`.
pub struct Decoder<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.b.len() - self.pos
    }
    pub fn pos(&self) -> usize {
        self.pos
    }
    pub fn len(&self) -> usize {
        self.b.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pos >= self.b.len()
    }
    /// A safe `Vec::with_capacity` bound for a decoded element count: never reserve for more elements
    /// than the remaining input could possibly supply (each element is at least `elem_min` bytes). A
    /// truncated frame with an attacker-chosen count then fails cleanly at the per-element read instead
    /// of forcing a huge up-front reservation.
    pub fn cap_count(&self, n: usize, elem_min: usize) -> usize {
        n.min(self.remaining() / elem_min.max(1))
    }
    /// Peek the next byte (the command tag) without consuming it. `None` at end of stream.
    pub fn peek_u8(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.b.len() {
            return Err(GpuError::ShortBuffer);
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn raw_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.take(n)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.u64()?))
    }
    pub fn f64_finite(&mut self, field: &'static str) -> Result<f64> {
        let v = self.f64()?;
        if v.is_finite() {
            Ok(v)
        } else {
            Err(GpuError::NonFinite(field))
        }
    }
    /// Decode an `f32` that the IR contract requires to be **finite** — a viewport/scissor coordinate, a
    /// clear color/depth, any render-state value that indexes into pixels or a transform. A NaN or ±∞
    /// there is a malformed payload; reject it at the wire boundary with a typed error so it can never
    /// reach a backend and poison a viewport/clear/matrix. `field` names the site for the diagnostic.
    pub fn f32_finite(&mut self, field: &'static str) -> Result<f32> {
        let v = self.f32()?;
        if v.is_finite() {
            Ok(v)
        } else {
            Err(GpuError::NonFinite(field))
        }
    }
    /// Decode a canonical boolean: only `0` and `1` are valid. A non-canonical byte (e.g. `2`) is a
    /// malformed payload and errors rather than being normalized to `true`, so producer bugs surface.
    pub fn bool(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            b => Err(GpuError::NonCanonicalBool(b)),
        }
    }
    pub fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    pub fn str(&mut self) -> Result<String> {
        let s = self.bytes()?;
        core::str::from_utf8(s)
            .map(|x| x.to_string())
            .map_err(|_| GpuError::Utf8)
    }
    pub fn words(&mut self) -> Result<Vec<u32>> {
        let n = self.u32()? as usize;
        // Cap the pre-allocation at what the buffer can actually supply (4 bytes/word): a truncated
        // frame with an attacker-chosen `n` must not force a giant reserve before the loop's bounds
        // check fires. Excess `n` errors cleanly as ShortBuffer.
        let mut v = Vec::with_capacity(n.min(self.remaining() / 4));
        for _ in 0..n {
            v.push(self.u32()?);
        }
        Ok(v)
    }
    /// Read a length-prefixed sub-frame written by [`Encoder::frame`] and decode it with `f`, requiring
    /// `f` to consume the **entire** body. A frame whose declared length exceeds the bytes its content
    /// needs (trailing garbage after a valid message) is a malformed frame and errors as `TrailingBytes`
    /// rather than being silently normalized — strict producers/consumers stay in sync.
    pub fn frame<T, F: FnOnce(&mut Decoder) -> Result<T>>(&mut self, f: F) -> Result<T> {
        let body = self.bytes()?;
        let mut inner = Decoder::new(body);
        let t = f(&mut inner)?;
        if !inner.is_empty() {
            return Err(GpuError::TrailingBytes);
        }
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_roundtrip() {
        let mut e = Encoder::new();
        e.words(&[1, 2, 3, 0xDEAD_BEEF]);
        let bytes = e.into_vec();
        let mut d = Decoder::new(&bytes);
        assert_eq!(d.words().unwrap(), vec![1, 2, 3, 0xDEAD_BEEF]);
    }

    #[test]
    fn bool_rejects_non_canonical_bytes() {
        assert_eq!(Decoder::new(&[0]).bool(), Ok(false));
        assert_eq!(Decoder::new(&[1]).bool(), Ok(true));
        assert_eq!(
            Decoder::new(&[2]).bool(),
            Err(GpuError::NonCanonicalBool(2))
        );
        assert_eq!(
            Decoder::new(&[0xff]).bool(),
            Err(GpuError::NonCanonicalBool(0xff))
        );
    }

    #[test]
    fn frame_rejects_trailing_bytes() {
        let mut e = Encoder::new();
        e.frame(|inner| {
            inner.u32(7);
            inner.u8(0xAA); // extra trailing byte inside the frame body
        });
        let bytes = e.into_vec();
        let mut d = Decoder::new(&bytes);
        let r: Result<u32> = d.frame(|inner| inner.u32());
        assert_eq!(r, Err(GpuError::TrailingBytes));

        let mut e2 = Encoder::new();
        e2.frame(|inner| inner.u32(7));
        let good = e2.into_vec();
        let mut d2 = Decoder::new(&good);
        assert_eq!(d2.frame(|inner| inner.u32()), Ok(7));
    }

    #[test]
    fn words_bogus_count_is_short_buffer_not_oom() {
        let buf = [0xFFu8, 0xFF, 0xFF, 0xFF]; // count = 0xFFFF_FFFF, zero words follow
        let mut d = Decoder::new(&buf);
        assert!(matches!(d.words(), Err(GpuError::ShortBuffer)));
    }
}
