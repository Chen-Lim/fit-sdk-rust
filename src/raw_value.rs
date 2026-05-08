//! Raw (untransformed) field values produced by the M4 decoder.
//!
//! Each variant corresponds to a base type from [`BaseType`]. Values are
//! stored as `Vec<T>` regardless of cardinality — a scalar is just a
//! length-1 vec. Arrays are decoded element-by-element using the endianness
//! from the Definition message; a field collapses to [`RawValue::Invalid`]
//! when **every** element matches that type's invalid sentinel
//! (per protocol §3.1).
//!
//! Notable per-type rules:
//! - **Z series** (`UInt8z` / `UInt16z` / `UInt32z` / `UInt64z`): invalid
//!   sentinel is **`0`**, not all-ones.
//! - **`Byte`**: invalid only when *every* byte is `0xFF` (a standalone
//!   `0xFF` inside a multi-byte array is **valid**).
//! - **`String`**: invalid if first byte is `0x00` or empty after
//!   null-stripping. Decoded with `from_utf8_lossy` so malformed UTF-8 does
//!   not poison an entire message.
//! - **Float32/64**: invalid is the all-ones **bit pattern**, regardless of
//!   how it interprets as `f32`/`f64` (which would be a NaN payload).

use crate::base_type::BaseType;
use crate::error::FitError;
use crate::stream::Endian;

/// A decoded field value.
#[derive(Debug, Clone, PartialEq)]
pub enum RawValue {
    /// All elements matched the type's invalid sentinel.
    Invalid,

    /// Unsigned 8-bit enum value.
    Enum(Vec<u8>),
    /// Signed 8-bit integer.
    SInt8(Vec<i8>),
    /// Unsigned 8-bit integer.
    UInt8(Vec<u8>),
    /// Signed 16-bit integer.
    SInt16(Vec<i16>),
    /// Unsigned 16-bit integer.
    UInt16(Vec<u16>),
    /// Signed 32-bit integer.
    SInt32(Vec<i32>),
    /// Unsigned 32-bit integer.
    UInt32(Vec<u32>),
    /// UTF-8 string with the trailing `0x00` (and any padding) stripped.
    String(String),
    /// 32-bit IEEE 754 float.
    Float32(Vec<f32>),
    /// 64-bit IEEE 754 float.
    Float64(Vec<f64>),
    /// Unsigned 8-bit integer (invalid sentinel is 0, not 0xFF).
    UInt8z(Vec<u8>),
    /// Unsigned 16-bit integer (invalid sentinel is 0).
    UInt16z(Vec<u16>),
    /// Unsigned 32-bit integer (invalid sentinel is 0).
    UInt32z(Vec<u32>),
    /// Opaque byte array.
    Byte(Vec<u8>),
    /// Signed 64-bit integer.
    SInt64(Vec<i64>),
    /// Unsigned 64-bit integer.
    UInt64(Vec<u64>),
    /// Unsigned 64-bit integer (invalid sentinel is 0).
    UInt64z(Vec<u64>),
}

impl RawValue {
    /// True iff the field collapsed to the invalid sentinel.
    #[inline]
    pub fn is_invalid(&self) -> bool {
        matches!(self, RawValue::Invalid)
    }

    /// Get a scalar `u32` (works for `UInt32` and `UInt32z` length-1 fields).
    pub fn as_u32(&self) -> Option<u32> {
        match self {
            RawValue::UInt32(v) | RawValue::UInt32z(v) if v.len() == 1 => Some(v[0]),
            _ => None,
        }
    }

    /// Get a scalar `u16` (also widens `UInt8`/`Enum`).
    pub fn as_u16(&self) -> Option<u16> {
        match self {
            RawValue::UInt16(v) | RawValue::UInt16z(v) if v.len() == 1 => Some(v[0]),
            RawValue::UInt8(v) | RawValue::UInt8z(v) | RawValue::Enum(v) if v.len() == 1 => {
                Some(v[0] as u16)
            }
            _ => None,
        }
    }

    /// Get a scalar `u8` for length-1 single-byte fields.
    pub fn as_u8(&self) -> Option<u8> {
        match self {
            RawValue::UInt8(v) | RawValue::UInt8z(v) | RawValue::Enum(v) | RawValue::Byte(v)
                if v.len() == 1 =>
            {
                Some(v[0])
            }
            _ => None,
        }
    }

    /// Borrow the underlying string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            RawValue::String(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

// ───────────────────────────────────────────────────────────────────
// Decode entry point.
// ───────────────────────────────────────────────────────────────────

/// Decode a single field's bytes into a [`RawValue`].
///
/// `raw` must be exactly the field's declared wire size. Returns
/// [`FitError::MalformedField`] when a multi-byte type's size is not a
/// multiple of its element stride.
pub(crate) fn decode_value(
    base_type: BaseType,
    raw: &[u8],
    endian: Endian,
    field_def_num: u8,
) -> Result<RawValue, FitError> {
    let stride = base_type.element_size();
    // STRING and BYTE are stride-1 by construction; all other types must
    // have a wire size that is a positive multiple of their element size.
    if !base_type.is_string() && !base_type.is_byte() && (raw.is_empty() || raw.len() % stride != 0)
    {
        return Err(FitError::MalformedField {
            field_def_num,
            size: raw.len() as u8,
            element_size: stride,
        });
    }

    Ok(match base_type {
        BaseType::Enum => collapse_or(decode_u8(raw), |&v| v == 0xFF, RawValue::Enum),
        BaseType::UInt8 => collapse_or(decode_u8(raw), |&v| v == 0xFF, RawValue::UInt8),
        BaseType::UInt8z => collapse_or(decode_u8(raw), |&v| v == 0x00, RawValue::UInt8z),
        BaseType::SInt8 => collapse_or(decode_i8(raw), |&v| v == i8::MAX, RawValue::SInt8),
        BaseType::Byte => decode_byte(raw),
        BaseType::String => decode_string(raw),

        BaseType::UInt16 => collapse_or(
            decode_u16(raw, endian),
            |&v| v == u16::MAX,
            RawValue::UInt16,
        ),
        BaseType::UInt16z => collapse_or(decode_u16(raw, endian), |&v| v == 0, RawValue::UInt16z),
        BaseType::SInt16 => collapse_or(
            decode_i16(raw, endian),
            |&v| v == i16::MAX,
            RawValue::SInt16,
        ),

        BaseType::UInt32 => collapse_or(
            decode_u32(raw, endian),
            |&v| v == u32::MAX,
            RawValue::UInt32,
        ),
        BaseType::UInt32z => collapse_or(decode_u32(raw, endian), |&v| v == 0, RawValue::UInt32z),
        BaseType::SInt32 => collapse_or(
            decode_i32(raw, endian),
            |&v| v == i32::MAX,
            RawValue::SInt32,
        ),

        BaseType::UInt64 => collapse_or(
            decode_u64(raw, endian),
            |&v| v == u64::MAX,
            RawValue::UInt64,
        ),
        BaseType::UInt64z => collapse_or(decode_u64(raw, endian), |&v| v == 0, RawValue::UInt64z),
        BaseType::SInt64 => collapse_or(
            decode_i64(raw, endian),
            |&v| v == i64::MAX,
            RawValue::SInt64,
        ),

        BaseType::Float32 => collapse_or(
            decode_f32(raw, endian),
            |v| v.to_bits() == 0xFFFF_FFFF,
            RawValue::Float32,
        ),
        BaseType::Float64 => collapse_or(
            decode_f64(raw, endian),
            |v| v.to_bits() == 0xFFFF_FFFF_FFFF_FFFF,
            RawValue::Float64,
        ),
    })
}

// ───────────────────────────────────────────────────────────────────
// Per-type decoders. Each returns Vec<T>; invalid detection happens in
// `collapse_or` after decoding so the rule "every element invalid" is
// uniform across types.
// ───────────────────────────────────────────────────────────────────

fn decode_u8(raw: &[u8]) -> Vec<u8> {
    raw.to_vec()
}

fn decode_i8(raw: &[u8]) -> Vec<i8> {
    raw.iter().map(|&b| b as i8).collect()
}

fn decode_u16(raw: &[u8], endian: Endian) -> Vec<u16> {
    raw.chunks_exact(2)
        .map(|c| {
            let arr = [c[0], c[1]];
            match endian {
                Endian::Little => u16::from_le_bytes(arr),
                Endian::Big => u16::from_be_bytes(arr),
            }
        })
        .collect()
}

fn decode_i16(raw: &[u8], endian: Endian) -> Vec<i16> {
    raw.chunks_exact(2)
        .map(|c| {
            let arr = [c[0], c[1]];
            match endian {
                Endian::Little => i16::from_le_bytes(arr),
                Endian::Big => i16::from_be_bytes(arr),
            }
        })
        .collect()
}

fn decode_u32(raw: &[u8], endian: Endian) -> Vec<u32> {
    raw.chunks_exact(4)
        .map(|c| {
            let arr = [c[0], c[1], c[2], c[3]];
            match endian {
                Endian::Little => u32::from_le_bytes(arr),
                Endian::Big => u32::from_be_bytes(arr),
            }
        })
        .collect()
}

fn decode_i32(raw: &[u8], endian: Endian) -> Vec<i32> {
    raw.chunks_exact(4)
        .map(|c| {
            let arr = [c[0], c[1], c[2], c[3]];
            match endian {
                Endian::Little => i32::from_le_bytes(arr),
                Endian::Big => i32::from_be_bytes(arr),
            }
        })
        .collect()
}

fn decode_u64(raw: &[u8], endian: Endian) -> Vec<u64> {
    raw.chunks_exact(8)
        .map(|c| {
            let arr = [c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]];
            match endian {
                Endian::Little => u64::from_le_bytes(arr),
                Endian::Big => u64::from_be_bytes(arr),
            }
        })
        .collect()
}

fn decode_i64(raw: &[u8], endian: Endian) -> Vec<i64> {
    raw.chunks_exact(8)
        .map(|c| {
            let arr = [c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]];
            match endian {
                Endian::Little => i64::from_le_bytes(arr),
                Endian::Big => i64::from_be_bytes(arr),
            }
        })
        .collect()
}

fn decode_f32(raw: &[u8], endian: Endian) -> Vec<f32> {
    raw.chunks_exact(4)
        .map(|c| {
            let arr = [c[0], c[1], c[2], c[3]];
            match endian {
                Endian::Little => f32::from_le_bytes(arr),
                Endian::Big => f32::from_be_bytes(arr),
            }
        })
        .collect()
}

fn decode_f64(raw: &[u8], endian: Endian) -> Vec<f64> {
    raw.chunks_exact(8)
        .map(|c| {
            let arr = [c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]];
            match endian {
                Endian::Little => f64::from_le_bytes(arr),
                Endian::Big => f64::from_be_bytes(arr),
            }
        })
        .collect()
}

/// Build a [`RawValue`] from a decoded vec, collapsing to
/// [`RawValue::Invalid`] when every element matches `is_invalid`.
fn collapse_or<T, F>(values: Vec<T>, is_invalid: F, ctor: fn(Vec<T>) -> RawValue) -> RawValue
where
    F: Fn(&T) -> bool,
{
    if values.iter().all(is_invalid) {
        RawValue::Invalid
    } else {
        ctor(values)
    }
}

/// Special handling for [`BaseType::Byte`]: invalid only when every byte is
/// `0xFF`. (Reusing `collapse_or` works here because the per-element rule
/// happens to coincide — but we keep the explicit predicate for clarity.)
fn decode_byte(raw: &[u8]) -> RawValue {
    let v = raw.to_vec();
    if !v.is_empty() && v.iter().all(|&b| b == 0xFF) {
        RawValue::Invalid
    } else {
        RawValue::Byte(v)
    }
}

fn decode_string(raw: &[u8]) -> RawValue {
    // FIT strings are null-terminated UTF-8 with possible trailing padding.
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    if end == 0 {
        return RawValue::Invalid;
    }
    let s = String::from_utf8_lossy(&raw[..end]).into_owned();
    RawValue::String(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(bt: BaseType, raw: &[u8], endian: Endian) -> RawValue {
        decode_value(bt, raw, endian, 0).unwrap()
    }

    #[test]
    fn enum_invalid_when_all_ff() {
        assert_eq!(
            dec(BaseType::Enum, &[0xFF], Endian::Little),
            RawValue::Invalid
        );
        assert_eq!(
            dec(BaseType::Enum, &[0xFF, 0xFF], Endian::Little),
            RawValue::Invalid
        );
    }

    #[test]
    fn enum_valid_keeps_all_elements() {
        // Mixed: 0xFF stays in the array because at least one element is non-FF.
        assert_eq!(
            dec(BaseType::Enum, &[1, 0xFF, 4], Endian::Little),
            RawValue::Enum(vec![1, 0xFF, 4])
        );
    }

    #[test]
    fn uint8z_invalid_is_zero_not_ff() {
        assert_eq!(
            dec(BaseType::UInt8z, &[0], Endian::Little),
            RawValue::Invalid
        );
        assert_eq!(
            dec(BaseType::UInt8z, &[0xFF], Endian::Little),
            RawValue::UInt8z(vec![0xFF])
        );
    }

    #[test]
    fn byte_invalid_only_when_all_ff() {
        // Single element 0xFF is invalid (matches "all FF" rule for length 1).
        assert_eq!(
            dec(BaseType::Byte, &[0xFF], Endian::Little),
            RawValue::Invalid
        );
        // Mixed: any non-FF byte makes the field valid.
        assert_eq!(
            dec(BaseType::Byte, &[0xFF, 0x01, 0xFF], Endian::Little),
            RawValue::Byte(vec![0xFF, 0x01, 0xFF])
        );
        // All-FF multi-byte: invalid.
        assert_eq!(
            dec(BaseType::Byte, &[0xFF, 0xFF, 0xFF], Endian::Little),
            RawValue::Invalid
        );
    }

    #[test]
    fn uint16_endianness_and_invalid() {
        // 0x1234 in LE = [0x34, 0x12]; in BE = [0x12, 0x34].
        assert_eq!(
            dec(BaseType::UInt16, &[0x34, 0x12], Endian::Little),
            RawValue::UInt16(vec![0x1234])
        );
        assert_eq!(
            dec(BaseType::UInt16, &[0x12, 0x34], Endian::Big),
            RawValue::UInt16(vec![0x1234])
        );
        // All 0xFFFF → invalid.
        assert_eq!(
            dec(BaseType::UInt16, &[0xFF, 0xFF], Endian::Little),
            RawValue::Invalid
        );
    }

    #[test]
    fn uint32_le_decodes_known_timestamp() {
        // 995749880 = 0x3B59EFF8 (FIT epoch seconds — matches the first
        // `record` message's timestamp in Activity.fit).
        assert_eq!(
            dec(BaseType::UInt32, &[0xF8, 0xEF, 0x59, 0x3B], Endian::Little),
            RawValue::UInt32(vec![995749880])
        );
    }

    #[test]
    fn float32_invalid_is_all_ones_bit_pattern() {
        assert_eq!(
            dec(BaseType::Float32, &[0xFF, 0xFF, 0xFF, 0xFF], Endian::Little),
            RawValue::Invalid
        );
        // 1.0 = 0x3F800000 → LE bytes [0x00, 0x00, 0x80, 0x3F]
        let v = dec(BaseType::Float32, &[0x00, 0x00, 0x80, 0x3F], Endian::Little);
        match v {
            RawValue::Float32(arr) => assert_eq!(arr, vec![1.0]),
            _ => panic!("expected Float32"),
        }
    }

    #[test]
    fn string_strips_null_and_lossy_decodes() {
        assert_eq!(
            dec(BaseType::String, b"FIT Cookbook\0\0\0", Endian::Little),
            RawValue::String("FIT Cookbook".into())
        );
        assert_eq!(
            dec(BaseType::String, b"\0", Endian::Little),
            RawValue::Invalid
        );
        assert_eq!(
            dec(BaseType::String, b"", Endian::Little),
            RawValue::Invalid
        );
    }

    #[test]
    fn malformed_size_returns_error() {
        // UInt32 with 3 bytes is malformed.
        let err = decode_value(BaseType::UInt32, &[0, 0, 0], Endian::Little, 42).unwrap_err();
        assert!(matches!(
            err,
            FitError::MalformedField {
                field_def_num: 42,
                ..
            }
        ));
    }

    #[test]
    fn helpers_extract_scalars() {
        let v = RawValue::UInt32(vec![995749880]);
        assert_eq!(v.as_u32(), Some(995749880));
        assert!(!v.is_invalid());

        let v = RawValue::Enum(vec![4]);
        assert_eq!(v.as_u8(), Some(4));
        assert_eq!(v.as_u16(), Some(4));

        assert!(RawValue::Invalid.is_invalid());
        assert_eq!(RawValue::Invalid.as_u32(), None);
    }

    #[test]
    fn sint8_invalid_at_max() {
        assert_eq!(
            dec(BaseType::SInt8, &[0x7F], Endian::Little),
            RawValue::Invalid
        );
        assert_eq!(
            dec(BaseType::SInt8, &[0x7E], Endian::Little),
            RawValue::SInt8(vec![126])
        );
    }

    #[test]
    fn sint16_array_partial_invalid_keeps_field() {
        // Mix valid + invalid sentinels: the field as a whole is valid because
        // at least one element is real. (Per-element invalidity is not modeled
        // at this layer — M5 transforms can interpret if needed.)
        assert_eq!(
            dec(
                BaseType::SInt16,
                &[0xFF, 0x7F, 0x05, 0x00], // [i16::MAX, 5]
                Endian::Little
            ),
            RawValue::SInt16(vec![i16::MAX, 5])
        );
    }
}
