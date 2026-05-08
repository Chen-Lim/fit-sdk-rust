#![deny(unsafe_code)]

//! Garmin FIT protocol SDK for Rust — pure-Rust decoder + encoder for v21.200
//! `.fit` files (workouts, activities, courses, settings, etc.).
//!
//! # Quick start
//!
//! ```no_run
//! use fit::{Decoder, Encoder};
//!
//! let bytes = std::fs::read("Activity.fit")?;
//!
//! // Decode every message with all transforms applied (DateTime, enum
//! // strings, scale/offset, components, sub-fields, dev fields).
//! let (messages, errors) = Decoder::builder(&bytes).build().read_all();
//! assert!(errors.is_empty());
//!
//! // Encode the typed messages back into a FIT binary.
//! let encoded: Vec<u8> = Encoder::new().encode(&messages)?;
//! fit::check_integrity(&encoded)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Module map
//!
//! | API | Purpose |
//! |---|---|
//! | [`is_fit`] / [`check_integrity`] | Quick file-level validation |
//! | [`FileHeader`] / [`ByteStream`] | Low-level header + byte cursor |
//! | [`Decoder`] / [`RawMessage`] | Streaming raw-message iterator |
//! | [`DecoderBuilder`] / [`TypedDecoder`] / [`Message`] | Profile-aware typed pipeline |
//! | [`Encoder`] / [`EncoderBuilder`] | Round-trip back to FIT binary |
//! | [`crc`] / [`base_type`] / [`record_header`] | Protocol primitives |
//! | [`profile`] | Generated Profile.xlsx tables (see `MesgNum`, `MesgInfo`, `FieldInfo`) |
//! | [`transforms`] | Re-exports of the M5 transform helpers (datetime / enum / scale / components) |
//! | [`merge_heart_rates`] / [`decode_memo_glob`] | M6 post-processing helpers |
//! | [`FitError`] | Single error type used by every fallible operation |
//!
//! See the [repository](https://github.com/Chen-Lim/fit-editor-rust) for the
//! milestone log and companion tools.

pub mod base_type;
pub mod crc;
pub mod datetime;
mod decoder;
mod definition;
pub mod dev_fields;
mod encoder;
mod error;
mod header;
pub mod output_stream;
pub mod profile;
mod raw_value;
mod record_header;
mod stream;
pub mod transforms;
mod typed_decoder;
mod value;

pub use base_type::BaseType;
pub use decoder::{Decoder, RawDevField, RawField, RawMessage};
pub use encoder::{Encoder, EncoderBuilder};
pub use definition::{
    DeveloperFieldDefinition, FieldDefinition, LocalDefinitions, MessageDefinition,
    LOCAL_DEFINITION_SLOTS,
};
pub use dev_fields::{DevFieldInfo, DevFieldRegistry};
pub use error::FitError;
pub use header::FileHeader;
pub use raw_value::RawValue;
pub use record_header::RecordHeader;
pub use stream::{ByteStream, Endian};
pub use typed_decoder::{DecoderBuilder, TransformOptions, TypedDecoder};
pub use transforms::decode_memo_glob;
pub use transforms::merge_heart_rates;
pub use value::{Field, FieldKind, Message, Value};

/// Compute the CRC-16 over a byte slice. Thin wrapper around [`crc::calculate`].
pub fn crc16(data: &[u8]) -> u16 {
    crc::calculate(data)
}

/// Quick check: does this byte slice plausibly hold a FIT file?
///
/// Returns `true` iff:
/// 1. Length is at least 12 bytes
/// 2. Byte 0 (header size) is 12 or 14
/// 3. The total slice is at least `header_size + 2` bytes long (room for the
///    trailing file CRC)
/// 4. Bytes 8..12 equal the ASCII string `".FIT"`
///
/// This does **not** verify any CRC; for that use [`check_integrity`].
pub fn is_fit(bytes: &[u8]) -> bool {
    let Ok(header) = FileHeader::parse(bytes) else {
        return false;
    };
    bytes.len() >= header.header_size as usize + 2
}

/// Fully verify a FIT file's CRCs.
///
/// Performs three checks in order:
/// 1. The header parses (size, signature)
/// 2. If the 14-byte header carries a non-zero CRC, it matches the CRC of
///    the first 12 bytes (per protocol, a stored value of `0` skips this check)
/// 3. The two trailing bytes match the CRC of the entire header + data region
///
/// The byte slice must be **at least** `header.total_file_size()` bytes long.
/// Anything beyond that (e.g. additional chained FIT files) is ignored here.
pub fn check_integrity(bytes: &[u8]) -> Result<(), FitError> {
    let header = FileHeader::parse(bytes)?;
    let total = header.total_file_size();
    if bytes.len() < total {
        return Err(FitError::TooShort {
            expected: total,
            actual: bytes.len(),
        });
    }

    // 1. Header CRC (only present in 14-byte headers, only checked when non-zero).
    if let Some(stored) = header.header_crc {
        if stored != 0 {
            let calculated = crc::calculate(&bytes[..12]);
            if stored != calculated {
                return Err(FitError::HeaderCrcMismatch { stored, calculated });
            }
        }
    }

    // 2. Trailing file CRC (always little-endian u16).
    let crc_offset = header.file_crc_offset();
    let stored_file_crc = u16::from_le_bytes([bytes[crc_offset], bytes[crc_offset + 1]]);
    let calculated_file_crc = crc::calculate(&bytes[..crc_offset]);
    if stored_file_crc != calculated_file_crc {
        return Err(FitError::FileCrcMismatch {
            stored: stored_file_crc,
            calculated: calculated_file_crc,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_fit_rejects_too_short() {
        assert!(!is_fit(&[]));
        assert!(!is_fit(&[14u8]));
    }

    #[test]
    fn is_fit_rejects_bad_signature() {
        let mut bytes = [0u8; 16];
        bytes[0] = 14;
        bytes[8..12].copy_from_slice(b"NOPE");
        assert!(!is_fit(&bytes));
    }

    #[test]
    fn is_fit_rejects_invalid_header_size_byte() {
        let mut bytes = [0u8; 16];
        bytes[0] = 16; // not 12 or 14
        bytes[8..12].copy_from_slice(b".FIT");
        assert!(!is_fit(&bytes));
    }

    #[test]
    fn is_fit_accepts_well_formed_14_byte() {
        // 14-byte header + 2 trailing bytes (CRC value doesn't matter for is_fit).
        let mut bytes = [0u8; 16];
        bytes[0] = 14;
        bytes[8..12].copy_from_slice(b".FIT");
        assert!(is_fit(&bytes));
    }
}
