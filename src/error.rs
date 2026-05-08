//! Error types for the FIT crate.
//!
//! All public APIs return `Result<_, FitError>`. The variants are intentionally
//! specific so callers can distinguish *why* a file was rejected (truncated vs.
//! wrong signature vs. CRC mismatch) without parsing error message strings.

use thiserror::Error;

/// Anything that can go wrong while reading a FIT file.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FitError {
    /// The byte slice is shorter than required for the operation in progress.
    #[error("file too short: needed at least {expected} bytes, got {actual}")]
    TooShort {
        /// Minimum number of bytes required.
        expected: usize,
        /// Actual number of bytes available.
        actual: usize,
    },

    /// The header size byte (offset 0) is neither 12 nor 14.
    #[error("invalid header size byte: expected 12 or 14, got {0}")]
    InvalidHeaderSize(u8),

    /// The 4-byte signature at offset 8..12 is not the ASCII string `".FIT"`.
    #[error("invalid FIT signature at offset 8..12: got {0:?}")]
    InvalidSignature([u8; 4]),

    /// The 14-byte header carries a non-zero CRC that does not match the
    /// CRC computed over its own first 12 bytes. Per protocol, a stored CRC
    /// of `0x0000` is treated as "skip verification" (legacy firmware) and
    /// will *not* trigger this error.
    #[error("header CRC mismatch: stored 0x{stored:04X}, calculated 0x{calculated:04X}")]
    HeaderCrcMismatch {
        /// CRC value stored in the header.
        stored: u16,
        /// CRC computed over the header bytes.
        calculated: u16,
    },

    /// The two trailing CRC bytes do not match the CRC computed over the
    /// header + data region preceding them.
    #[error("file CRC mismatch: stored 0x{stored:04X}, calculated 0x{calculated:04X}")]
    FileCrcMismatch {
        /// CRC value stored in the file trailer.
        stored: u16,
        /// CRC computed over header + data region.
        calculated: u16,
    },

    /// Attempted to read past the end of the byte stream.
    #[error("unexpected end of stream at offset {offset}")]
    UnexpectedEof {
        /// Byte offset where the read failed.
        offset: usize,
    },

    /// A field-definition byte's type code (after `& 0x1F`) does not match
    /// any of the 17 known base types.
    #[error("unknown base type byte 0x{0:02X} (after masking, code 0x{1:02X})")]
    UnknownBaseType(u8, u8),

    /// A Data message references a local message number that has not been
    /// declared by a preceding Definition message.
    #[error("data message uses undefined local message number {0}")]
    UndefinedLocalMesgNum(u8),

    /// The reserved byte at offset 1 of a Definition message was non-zero.
    /// We don't error on this in production parsing (just a warning), but the
    /// variant exists for strict-mode tooling.
    #[error("non-zero reserved byte in definition message: 0x{0:02X}")]
    NonZeroReserved(u8),

    /// A field's wire size is not a multiple of its base type's element size,
    /// so the byte stream cannot be cleanly partitioned into elements.
    #[error(
        "malformed field {field_def_num}: size {size} not a multiple of base type element size {element_size}"
    )]
    MalformedField {
        /// Field definition number that failed validation.
        field_def_num: u8,
        /// Wire size in bytes.
        size: u8,
        /// Base type element size in bytes.
        element_size: usize,
    },

    /// The encoder was asked to emit more than 16 distinct global message
    /// numbers in a single FIT segment. Local definition slots are limited to
    /// 4 bits (0..=15) by the protocol; LRU eviction will arrive in M9.
    #[error("encoder: too many distinct mesg_nums ({0}); only 16 local definition slots are available")]
    TooManyLocalDefinitions(usize),

    /// A field's wire size or count exceeds 255 bytes (u8 limit).
    #[error("encoder: field too large for wire: {0} exceeds 255 byte limit")]
    FieldTooLarge(String),
}
