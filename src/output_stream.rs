//! Growing byte buffer with little-endian write helpers.
//!
//! Mirrors [`crate::stream::ByteStream`] on the write side. Used by the
//! encoder to assemble FIT binary (header + definition/data records + CRC).

/// A growable byte buffer for writing FIT binary.
#[derive(Debug, Default)]
pub struct OutputStream {
    buf: Vec<u8>,
}

impl OutputStream {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
        }
    }

    /// Current write position (length of the buffer).
    #[inline]
    pub fn position(&self) -> usize {
        self.buf.len()
    }

    /// Borrow the written bytes.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    /// Consume the stream and return the buffer.
    #[inline]
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    /// Write a single byte.
    #[inline]
    pub fn write_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    /// Write a u16 in little-endian byte order.
    #[inline]
    pub fn write_u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write a u32 in little-endian byte order.
    #[inline]
    pub fn write_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write a i16 in little-endian byte order.
    #[inline]
    pub fn write_i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write a i32 in little-endian byte order.
    #[inline]
    pub fn write_i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write a u64 in little-endian byte order.
    #[inline]
    pub fn write_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write a i64 in little-endian byte order.
    #[inline]
    pub fn write_i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write raw bytes.
    #[inline]
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Patch bytes at a specific offset (for backpatching header fields).
    #[inline]
    pub fn patch(&mut self, offset: usize, bytes: &[u8]) {
        self.buf[offset..offset + bytes.len()].copy_from_slice(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_primitives() {
        let mut s = OutputStream::new();
        s.write_u8(0x01);
        s.write_u16(0x0203);
        s.write_u32(0x04050607);
        assert_eq!(s.as_slice(), &[0x01, 0x03, 0x02, 0x07, 0x06, 0x05, 0x04]);
    }

    #[test]
    fn write_bytes_and_position() {
        let mut s = OutputStream::new();
        assert_eq!(s.position(), 0);
        s.write_bytes(b".FIT");
        assert_eq!(s.position(), 4);
        assert_eq!(s.as_slice(), b".FIT");
    }

    #[test]
    fn patch_overwrites() {
        let mut s = OutputStream::new();
        s.write_u32(0); // placeholder
        s.write_u32(0x12345678);
        s.patch(0, &0xAABBCCDDu32.to_le_bytes());
        assert_eq!(&s.as_slice()[0..4], &0xAABBCCDDu32.to_le_bytes());
        assert_eq!(&s.as_slice()[4..8], &0x12345678u32.to_le_bytes());
    }

    #[test]
    fn into_bytes_consumes() {
        let mut s = OutputStream::with_capacity(16);
        s.write_u8(42);
        let bytes = s.into_bytes();
        assert_eq!(bytes, vec![42]);
    }

    #[test]
    fn signed_types() {
        let mut s = OutputStream::new();
        s.write_i16(-1);
        s.write_i32(-2);
        assert_eq!(&s.as_slice()[0..2], &(-1i16).to_le_bytes());
        assert_eq!(&s.as_slice()[2..6], &(-2i32).to_le_bytes());
    }
}
