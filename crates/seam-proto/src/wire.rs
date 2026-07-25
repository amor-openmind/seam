//! Minimal big-endian read/write cursors.
//!
//! Hand-rolled on purpose: this crate owns the bytes on the wire, so the encoding must
//! be a property of *this source file* and not of some dependency's release notes.
//!
//! [`Writer`] borrows the caller's `Vec<u8>` so a hot loop can reuse one buffer forever
//! and never allocate (see `P3` in `docs/GOAL.md`).

use crate::Error;

/// Big-endian reader over a byte slice.
#[derive(Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    #[must_use]
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).ok_or(Error::Truncated)?;
        let slice = self.buf.get(self.pos..end).ok_or(Error::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, Error> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64(&mut self) -> Result<u64, Error> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    pub fn i32(&mut self) -> Result<i32, Error> {
        self.u32().map(u32::cast_signed)
    }

    pub fn i64(&mut self) -> Result<i64, Error> {
        self.u64().map(u64::cast_signed)
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        self.take(n)
    }

    /// Read a `u16`-length-prefixed UTF-8 string.
    pub fn string(&mut self) -> Result<&'a str, Error> {
        let len = self.u16()? as usize;
        let raw = self.take(len)?;
        core::str::from_utf8(raw).map_err(|_| Error::InvalidUtf8)
    }

    /// Assert the input is fully consumed.
    ///
    /// Trailing bytes are treated as a decode error rather than ignored: silently
    /// accepting them would let a version mismatch look like success.
    pub fn finish(self) -> Result<(), Error> {
        if self.remaining() == 0 { Ok(()) } else { Err(Error::TrailingBytes(self.remaining())) }
    }
}

/// Big-endian writer appending into a caller-owned buffer.
#[derive(Debug)]
pub struct Writer<'a> {
    buf: &'a mut Vec<u8>,
}

impl<'a> Writer<'a> {
    #[must_use]
    pub fn new(buf: &'a mut Vec<u8>) -> Self {
        Self { buf }
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn i32(&mut self, v: i32) {
        self.u32(v.cast_unsigned());
    }

    pub fn i64(&mut self, v: i64) {
        self.u64(v.cast_unsigned());
    }

    pub fn bytes(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }

    /// Write a `u16`-length-prefixed UTF-8 string.
    ///
    /// Returns [`Error::TooLong`] rather than truncating; a truncated hostname or
    /// screen name would produce a confusing mismatch far from its cause.
    pub fn string(&mut self, v: &str) -> Result<(), Error> {
        let len = u16::try_from(v.len()).map_err(|_| Error::TooLong)?;
        self.u16(len);
        self.bytes(v.as_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_scalars() {
        let mut buf = Vec::new();
        let mut w = Writer::new(&mut buf);
        w.u8(0xAB);
        w.u16(0xBEEF);
        w.u32(0xDEAD_BEEF);
        w.i32(-42);
        w.i64(i64::MIN);
        w.string("سلام").unwrap();

        let mut r = Reader::new(&buf);
        assert_eq!(r.u8().unwrap(), 0xAB);
        assert_eq!(r.u16().unwrap(), 0xBEEF);
        assert_eq!(r.u32().unwrap(), 0xDEAD_BEEF);
        assert_eq!(r.i32().unwrap(), -42);
        assert_eq!(r.i64().unwrap(), i64::MIN);
        assert_eq!(r.string().unwrap(), "سلام");
        r.finish().unwrap();
    }

    #[test]
    fn short_read_is_truncated_not_panic() {
        let buf = [0u8; 3];
        let mut r = Reader::new(&buf);
        assert!(matches!(r.u32(), Err(Error::Truncated)));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let buf = [1u8, 2, 3];
        let mut r = Reader::new(&buf);
        r.u8().unwrap();
        assert!(matches!(r.finish(), Err(Error::TrailingBytes(2))));
    }

    #[test]
    fn bad_utf8_is_rejected() {
        let buf = [0x00, 0x02, 0xFF, 0xFE];
        let mut r = Reader::new(&buf);
        assert!(matches!(r.string(), Err(Error::InvalidUtf8)));
    }
}
