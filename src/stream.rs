//! Byte-level cursor over a FIT file slice.
//!
//! [`ByteStream`] is a non-owning, position-tracking reader. Each `read_*`
//! advances the cursor; `peek_*` does not. All multi-byte readers come in
//! both little-endian and big-endian forms because FIT specifies endianness
//! per-message (via the architecture byte of each Definition Message).

use crate::error::FitError;

/// Endianness selector for multi-byte reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

/// A read cursor over a borrowed byte slice.
#[derive(Debug, Clone)]
pub struct ByteStream<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> ByteStream<'a> {
    /// Wrap a byte slice. Cursor starts at offset 0.
    #[inline]
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// Underlying slice (full length, ignoring cursor).
    #[inline]
    pub fn as_slice(&self) -> &'a [u8] {
        self.bytes
    }

    /// Current cursor offset.
    #[inline]
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Move the cursor to an absolute offset. Returns an error if past end.
    pub fn seek(&mut self, offset: usize) -> Result<(), FitError> {
        if offset > self.bytes.len() {
            return Err(FitError::UnexpectedEof { offset });
        }
        self.pos = offset;
        Ok(())
    }

    /// Number of bytes remaining after the cursor.
    #[inline]
    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    /// True if cursor is at or past end of slice.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    /// Read the next byte without advancing.
    pub fn peek_u8(&self) -> Result<u8, FitError> {
        self.bytes
            .get(self.pos)
            .copied()
            .ok_or(FitError::UnexpectedEof { offset: self.pos })
    }

    /// Read and consume the next byte.
    pub fn read_u8(&mut self) -> Result<u8, FitError> {
        let v = self.peek_u8()?;
        self.pos += 1;
        Ok(v)
    }

    /// Read and consume the next `n` bytes as a borrowed sub-slice.
    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], FitError> {
        if self.remaining() < n {
            return Err(FitError::UnexpectedEof { offset: self.pos });
        }
        let slice = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    /// Read and consume an exact `N`-byte array.
    pub fn read_array<const N: usize>(&mut self) -> Result<[u8; N], FitError> {
        let slice = self.read_bytes(N)?;
        // Length is guaranteed; conversion cannot fail.
        let mut arr = [0u8; N];
        arr.copy_from_slice(slice);
        Ok(arr)
    }

    /// Read u16 in the requested endianness.
    pub fn read_u16(&mut self, endian: Endian) -> Result<u16, FitError> {
        let bytes = self.read_array::<2>()?;
        Ok(match endian {
            Endian::Little => u16::from_le_bytes(bytes),
            Endian::Big => u16::from_be_bytes(bytes),
        })
    }

    /// Read u32 in the requested endianness.
    pub fn read_u32(&mut self, endian: Endian) -> Result<u32, FitError> {
        let bytes = self.read_array::<4>()?;
        Ok(match endian {
            Endian::Little => u32::from_le_bytes(bytes),
            Endian::Big => u32::from_be_bytes(bytes),
        })
    }

    /// Convenience: read u16 little-endian.
    #[inline]
    pub fn read_u16_le(&mut self) -> Result<u16, FitError> {
        self.read_u16(Endian::Little)
    }

    /// Convenience: read u32 little-endian.
    #[inline]
    pub fn read_u32_le(&mut self) -> Result<u32, FitError> {
        self.read_u32(Endian::Little)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_and_remaining() {
        let mut s = ByteStream::new(&[1, 2, 3, 4, 5]);
        assert_eq!(s.position(), 0);
        assert_eq!(s.remaining(), 5);
        let _ = s.read_u8().unwrap();
        assert_eq!(s.position(), 1);
        assert_eq!(s.remaining(), 4);
    }

    #[test]
    fn peek_does_not_advance() {
        let mut s = ByteStream::new(&[42]);
        assert_eq!(s.peek_u8().unwrap(), 42);
        assert_eq!(s.position(), 0);
        assert_eq!(s.read_u8().unwrap(), 42);
        assert_eq!(s.position(), 1);
    }

    #[test]
    fn read_past_end_errors_with_correct_offset() {
        let mut s = ByteStream::new(&[1, 2]);
        let _ = s.read_u8().unwrap();
        let _ = s.read_u8().unwrap();
        assert_eq!(
            s.read_u8(),
            Err(FitError::UnexpectedEof { offset: 2 }),
        );
    }

    #[test]
    fn endianness_is_correct() {
        let mut s = ByteStream::new(&[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(s.read_u16(Endian::Little).unwrap(), 0x3412);
        s.seek(0).unwrap();
        assert_eq!(s.read_u16(Endian::Big).unwrap(), 0x1234);
        s.seek(0).unwrap();
        assert_eq!(s.read_u32(Endian::Little).unwrap(), 0x7856_3412);
        s.seek(0).unwrap();
        assert_eq!(s.read_u32(Endian::Big).unwrap(), 0x1234_5678);
    }

    #[test]
    fn read_bytes_borrows_correct_range() {
        let data = [10, 20, 30, 40, 50];
        let mut s = ByteStream::new(&data);
        assert_eq!(s.read_bytes(2).unwrap(), &[10, 20]);
        assert_eq!(s.read_bytes(3).unwrap(), &[30, 40, 50]);
        assert!(s.is_empty());
    }

    #[test]
    fn seek_within_and_at_end_ok() {
        let mut s = ByteStream::new(&[1, 2, 3]);
        s.seek(3).unwrap(); // exact end is allowed
        assert!(s.is_empty());
        assert_eq!(s.seek(4), Err(FitError::UnexpectedEof { offset: 4 }));
    }
}
