//! FIT file header (12 or 14 bytes).
//!
//! Layout (all multi-byte integers little-endian):
//!
//! | Offset | Size | Field            | Meaning                                  |
//! |-------:|-----:|------------------|------------------------------------------|
//! |     0  |   1  | Header Size      | `0x0E` (14) or `0x0C` (12)              |
//! |     1  |   1  | Protocol Version | e.g. `0x20` for v2.0                    |
//! |     2  |   2  | Profile Version  | u16 LE, e.g. 21200                      |
//! |     4  |   4  | Data Size        | u32 LE, bytes of records (no header/CRC)|
//! |     8  |   4  | Signature        | ASCII `.FIT` = `[0x2E,0x46,0x49,0x54]`  |
//! |    12  |   2  | Header CRC       | u16 LE — only present in 14-byte header |
//!
//! Reference: `guide/fit_binary_learning_notes.md` §1.1.

use crate::error::FitError;

/// Parsed FIT file header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileHeader {
    /// Either 12 or 14.
    pub header_size: u8,
    /// Protocol version byte (high nibble = major, low = minor).
    pub protocol_version: u8,
    /// Profile version (e.g. 21200 = v21.200).
    pub profile_version: u16,
    /// Number of bytes in the records area (between header and trailing file CRC).
    pub data_size: u32,
    /// Header CRC, only present when `header_size == 14`.
    /// A stored value of `Some(0)` means "skip verification" (legacy firmware).
    pub header_crc: Option<u16>,
}

impl FileHeader {
    /// The four-byte ASCII signature at offset 8..12.
    pub const SIGNATURE: [u8; 4] = *b".FIT";
    /// Smallest valid header size.
    pub const MIN_SIZE: u8 = 12;
    /// Largest valid header size.
    pub const MAX_SIZE: u8 = 14;

    /// Parse a header from the start of `bytes`.
    ///
    /// The slice must hold at least `header_size` bytes (12 or 14). The
    /// signature `.FIT` is verified. The header CRC is **not** verified here;
    /// see [`crate::check_integrity`] for full validation.
    pub fn parse(bytes: &[u8]) -> Result<Self, FitError> {
        if bytes.len() < Self::MIN_SIZE as usize {
            return Err(FitError::TooShort {
                expected: Self::MIN_SIZE as usize,
                actual: bytes.len(),
            });
        }

        let header_size = bytes[0];
        if header_size != 12 && header_size != 14 {
            return Err(FitError::InvalidHeaderSize(header_size));
        }
        if bytes.len() < header_size as usize {
            return Err(FitError::TooShort {
                expected: header_size as usize,
                actual: bytes.len(),
            });
        }

        let protocol_version = bytes[1];
        let profile_version = u16::from_le_bytes([bytes[2], bytes[3]]);
        let data_size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);

        // Signature is unconditionally at 8..12 in both header sizes.
        let signature: [u8; 4] = [bytes[8], bytes[9], bytes[10], bytes[11]];
        if signature != Self::SIGNATURE {
            return Err(FitError::InvalidSignature(signature));
        }

        let header_crc = if header_size == 14 {
            Some(u16::from_le_bytes([bytes[12], bytes[13]]))
        } else {
            None
        };

        Ok(Self {
            header_size,
            protocol_version,
            profile_version,
            data_size,
            header_crc,
        })
    }

    /// Total expected file size: header + data + 2-byte trailing file CRC.
    #[inline]
    pub fn total_file_size(&self) -> usize {
        self.header_size as usize + self.data_size as usize + 2
    }

    /// Byte offset where the trailing file CRC begins.
    #[inline]
    pub fn file_crc_offset(&self) -> usize {
        self.header_size as usize + self.data_size as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid 14-byte header. `data_size` is in records-area
    /// bytes; `header_crc` is the literal value to store.
    fn make_14_byte_header(data_size: u32, header_crc: u16) -> [u8; 14] {
        let ds = data_size.to_le_bytes();
        let hc = header_crc.to_le_bytes();
        [
            14, 0x20, 0xD0, 0x52, // size, protocol, profile_version (21200 LE = D0 52)
            ds[0], ds[1], ds[2], ds[3], 0x2E, 0x46, 0x49, 0x54, // data_size, ".FIT"
            hc[0], hc[1],
        ]
    }

    #[test]
    fn parses_14_byte_header() {
        let bytes = make_14_byte_header(94070, 0xABCD);
        let h = FileHeader::parse(&bytes).unwrap();
        assert_eq!(h.header_size, 14);
        assert_eq!(h.protocol_version, 0x20);
        assert_eq!(h.profile_version, 21200);
        assert_eq!(h.data_size, 94070);
        assert_eq!(h.header_crc, Some(0xABCD));
        assert_eq!(h.total_file_size(), 14 + 94070 + 2);
    }

    #[test]
    fn parses_12_byte_header() {
        // Same shape but header_size=12, no CRC.
        let bytes: [u8; 12] = [12, 0x10, 0x10, 0x00, 0x40, 0, 0, 0, 0x2E, 0x46, 0x49, 0x54];
        let h = FileHeader::parse(&bytes).unwrap();
        assert_eq!(h.header_size, 12);
        assert_eq!(h.protocol_version, 0x10);
        assert_eq!(h.data_size, 0x40);
        assert_eq!(h.header_crc, None);
    }

    #[test]
    fn rejects_invalid_header_size() {
        let mut bytes = make_14_byte_header(0, 0);
        bytes[0] = 13; // not 12 or 14
        assert_eq!(FileHeader::parse(&bytes), Err(FitError::InvalidHeaderSize(13)));
    }

    #[test]
    fn rejects_invalid_signature() {
        let mut bytes = make_14_byte_header(0, 0);
        bytes[8] = b'X'; // corrupt signature
        let err = FileHeader::parse(&bytes).unwrap_err();
        assert!(matches!(err, FitError::InvalidSignature(sig) if sig[0] == b'X'));
    }

    #[test]
    fn rejects_too_short_for_minimum() {
        let bytes = [14u8; 11];
        assert_eq!(
            FileHeader::parse(&bytes),
            Err(FitError::TooShort {
                expected: 12,
                actual: 11,
            }),
        );
    }

    #[test]
    fn rejects_too_short_for_declared_14_byte() {
        // Says size 14 but only provides 12 bytes (just enough for signature check).
        let bytes: [u8; 12] = [14, 0x20, 0, 0, 0, 0, 0, 0, 0x2E, 0x46, 0x49, 0x54];
        assert_eq!(
            FileHeader::parse(&bytes),
            Err(FitError::TooShort {
                expected: 14,
                actual: 12,
            }),
        );
    }
}
