//! Raw (untransformed) field values produced by the M4 decoder.
//!
//! Each base type has **two** variants: a `…Scalar(T)` for the common case of
//! a length-1 field (~95% of FIT fields) and a `…Array(Box<[T]>)` for the rare
//! multi-element case. Scalar variants are stack-only — no heap allocation per
//! decoded message — while Array variants box the slice so the enum payload
//! stays compact (16B + tag) regardless of cardinality.
//!
//! Invalid detection still applies the rule "every element matches the
//! sentinel" (per protocol §3.1) but is checked before construction so the
//! caller never sees an `Invalid` array.
//!
//! Notable per-type rules:
//! - **Z series** (`U8z` / `U16z` / `U32z` / `U64z`): invalid sentinel is `0`.
//! - **`Byte`**: invalid only when *every* byte is `0xFF`.
//! - **`String`**: invalid if first byte is `0x00` or empty after
//!   null-stripping. Decoded with `from_utf8_lossy`.
//! - **`Float32/64`**: invalid is the all-ones bit pattern.

use crate::base_type::BaseType;
use crate::error::FitError;
use crate::stream::Endian;

/// A decoded field value.
#[derive(Debug, Clone, PartialEq)]
pub enum RawValue {
    /// All elements matched the type's invalid sentinel.
    Invalid,

    // ── Scalars (stack-only) ───────────────────────────────────────────
    EnumScalar(u8),
    U8Scalar(u8),
    U8zScalar(u8),
    I8Scalar(i8),
    U16Scalar(u16),
    U16zScalar(u16),
    I16Scalar(i16),
    U32Scalar(u32),
    U32zScalar(u32),
    I32Scalar(i32),
    U64Scalar(u64),
    U64zScalar(u64),
    I64Scalar(i64),
    F32Scalar(f32),
    F64Scalar(f64),

    // ── Arrays (heap-boxed, length ≥ 2) ────────────────────────────────
    EnumArray(Box<[u8]>),
    U8Array(Box<[u8]>),
    U8zArray(Box<[u8]>),
    I8Array(Box<[i8]>),
    U16Array(Box<[u16]>),
    U16zArray(Box<[u16]>),
    I16Array(Box<[i16]>),
    U32Array(Box<[u32]>),
    U32zArray(Box<[u32]>),
    I32Array(Box<[i32]>),
    U64Array(Box<[u64]>),
    U64zArray(Box<[u64]>),
    I64Array(Box<[i64]>),
    F32Array(Box<[f32]>),
    F64Array(Box<[f64]>),

    /// UTF-8 string with the trailing `0x00` (and any padding) stripped.
    String(Box<str>),
    /// Opaque byte array (single bytes that are not invalid sentinels also
    /// land here — there is no `ByteScalar` variant since `Byte` is by
    /// definition an array type on the wire).
    Byte(Box<[u8]>),
}

impl RawValue {
    /// True iff the field collapsed to the invalid sentinel.
    #[inline]
    pub fn is_invalid(&self) -> bool {
        matches!(self, RawValue::Invalid)
    }

    /// Borrow the underlying string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            RawValue::String(s) => Some(s.as_ref()),
            _ => None,
        }
    }

    /// Get a scalar `u8` for length-1 single-byte fields.
    pub fn as_u8(&self) -> Option<u8> {
        match *self {
            RawValue::U8Scalar(v) | RawValue::U8zScalar(v) | RawValue::EnumScalar(v) => Some(v),
            RawValue::Byte(ref b) if b.len() == 1 => Some(b[0]),
            _ => None,
        }
    }

    /// Get a scalar `u16` (also widens single-byte fields).
    pub fn as_u16(&self) -> Option<u16> {
        match *self {
            RawValue::U16Scalar(v) | RawValue::U16zScalar(v) => Some(v),
            RawValue::U8Scalar(v) | RawValue::U8zScalar(v) | RawValue::EnumScalar(v) => {
                Some(v as u16)
            }
            _ => None,
        }
    }

    /// Get a scalar `u32` (also accepts `U32z`).
    pub fn as_u32(&self) -> Option<u32> {
        match *self {
            RawValue::U32Scalar(v) | RawValue::U32zScalar(v) => Some(v),
            _ => None,
        }
    }

    /// Best-effort cast of any single-element numeric value to `u64`.
    /// Signed types are widened via two's complement (matches the prior
    /// `components::scalar_as_u64` behavior).
    pub fn scalar_u64(&self) -> Option<u64> {
        match *self {
            RawValue::EnumScalar(v) | RawValue::U8Scalar(v) | RawValue::U8zScalar(v) => {
                Some(v as u64)
            }
            RawValue::U16Scalar(v) | RawValue::U16zScalar(v) => Some(v as u64),
            RawValue::U32Scalar(v) | RawValue::U32zScalar(v) => Some(v as u64),
            RawValue::U64Scalar(v) | RawValue::U64zScalar(v) => Some(v),
            RawValue::I8Scalar(v) => Some(v as u8 as u64),
            RawValue::I16Scalar(v) => Some(v as u16 as u64),
            RawValue::I32Scalar(v) => Some(v as u32 as u64),
            RawValue::I64Scalar(v) => Some(v as u64),
            RawValue::Byte(ref b) if b.len() == 1 => Some(b[0] as u64),
            _ => None,
        }
    }

    /// Best-effort cast of any single-element numeric value to `f64`.
    pub fn scalar_f64(&self) -> Option<f64> {
        if let Some(u) = self.scalar_u64() {
            return Some(u as f64);
        }
        match *self {
            RawValue::F32Scalar(v) => Some(v as f64),
            RawValue::F64Scalar(v) => Some(v),
            _ => None,
        }
    }

    /// Iterate any numeric variant (scalar or array) as `f64`s. Returns
    /// `None` for non-numeric types (`String`, `Invalid`).
    pub fn to_f64s(&self) -> Option<Vec<f64>> {
        use RawValue::*;
        Some(match self {
            EnumScalar(v) | U8Scalar(v) | U8zScalar(v) => vec![*v as f64],
            U16Scalar(v) | U16zScalar(v) => vec![*v as f64],
            U32Scalar(v) | U32zScalar(v) => vec![*v as f64],
            U64Scalar(v) | U64zScalar(v) => vec![*v as f64],
            I8Scalar(v) => vec![*v as f64],
            I16Scalar(v) => vec![*v as f64],
            I32Scalar(v) => vec![*v as f64],
            I64Scalar(v) => vec![*v as f64],
            F32Scalar(v) => vec![*v as f64],
            F64Scalar(v) => vec![*v],
            EnumArray(a) | U8Array(a) | U8zArray(a) | Byte(a) => {
                a.iter().map(|&x| x as f64).collect()
            }
            U16Array(a) | U16zArray(a) => a.iter().map(|&x| x as f64).collect(),
            U32Array(a) | U32zArray(a) => a.iter().map(|&x| x as f64).collect(),
            U64Array(a) | U64zArray(a) => a.iter().map(|&x| x as f64).collect(),
            I8Array(a) => a.iter().map(|&x| x as f64).collect(),
            I16Array(a) => a.iter().map(|&x| x as f64).collect(),
            I32Array(a) => a.iter().map(|&x| x as f64).collect(),
            I64Array(a) => a.iter().map(|&x| x as f64).collect(),
            F32Array(a) => a.iter().map(|&x| x as f64).collect(),
            F64Array(a) => a.to_vec(),
            _ => return None,
        })
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
    if !base_type.is_string() && !base_type.is_byte() && (raw.is_empty() || raw.len() % stride != 0)
    {
        return Err(FitError::MalformedField {
            field_def_num,
            size: raw.len() as u8,
            element_size: stride,
        });
    }

    Ok(match base_type {
        BaseType::Enum => collapse(
            decode_u8_iter(raw),
            |&v| v == 0xFF,
            RawValue::EnumScalar,
            RawValue::EnumArray,
        ),
        BaseType::UInt8 => collapse(
            decode_u8_iter(raw),
            |&v| v == 0xFF,
            RawValue::U8Scalar,
            RawValue::U8Array,
        ),
        BaseType::UInt8z => collapse(
            decode_u8_iter(raw),
            |&v| v == 0x00,
            RawValue::U8zScalar,
            RawValue::U8zArray,
        ),
        BaseType::SInt8 => collapse(
            raw.iter().map(|&b| b as i8),
            |&v| v == i8::MAX,
            RawValue::I8Scalar,
            RawValue::I8Array,
        ),
        BaseType::Byte => decode_byte(raw),
        BaseType::String => decode_string(raw),

        BaseType::UInt16 => collapse(
            decode_u16_iter(raw, endian),
            |&v| v == u16::MAX,
            RawValue::U16Scalar,
            RawValue::U16Array,
        ),
        BaseType::UInt16z => collapse(
            decode_u16_iter(raw, endian),
            |&v| v == 0,
            RawValue::U16zScalar,
            RawValue::U16zArray,
        ),
        BaseType::SInt16 => collapse(
            decode_i16_iter(raw, endian),
            |&v| v == i16::MAX,
            RawValue::I16Scalar,
            RawValue::I16Array,
        ),

        BaseType::UInt32 => collapse(
            decode_u32_iter(raw, endian),
            |&v| v == u32::MAX,
            RawValue::U32Scalar,
            RawValue::U32Array,
        ),
        BaseType::UInt32z => collapse(
            decode_u32_iter(raw, endian),
            |&v| v == 0,
            RawValue::U32zScalar,
            RawValue::U32zArray,
        ),
        BaseType::SInt32 => collapse(
            decode_i32_iter(raw, endian),
            |&v| v == i32::MAX,
            RawValue::I32Scalar,
            RawValue::I32Array,
        ),

        BaseType::UInt64 => collapse(
            decode_u64_iter(raw, endian),
            |&v| v == u64::MAX,
            RawValue::U64Scalar,
            RawValue::U64Array,
        ),
        BaseType::UInt64z => collapse(
            decode_u64_iter(raw, endian),
            |&v| v == 0,
            RawValue::U64zScalar,
            RawValue::U64zArray,
        ),
        BaseType::SInt64 => collapse(
            decode_i64_iter(raw, endian),
            |&v| v == i64::MAX,
            RawValue::I64Scalar,
            RawValue::I64Array,
        ),

        BaseType::Float32 => collapse(
            decode_f32_iter(raw, endian),
            |v| v.to_bits() == 0xFFFF_FFFF,
            RawValue::F32Scalar,
            RawValue::F32Array,
        ),
        BaseType::Float64 => collapse(
            decode_f64_iter(raw, endian),
            |v| v.to_bits() == 0xFFFF_FFFF_FFFF_FFFF,
            RawValue::F64Scalar,
            RawValue::F64Array,
        ),
    })
}

// ───────────────────────────────────────────────────────────────────
// Per-type element iterators. Returning iterators (not Vec) lets
// `collapse` pick Scalar without ever allocating in the hot path.
// ───────────────────────────────────────────────────────────────────

fn decode_u8_iter(raw: &[u8]) -> impl Iterator<Item = u8> + '_ {
    raw.iter().copied()
}

fn decode_u16_iter(raw: &[u8], endian: Endian) -> impl Iterator<Item = u16> + '_ {
    raw.chunks_exact(2).map(move |c| {
        let arr = [c[0], c[1]];
        match endian {
            Endian::Little => u16::from_le_bytes(arr),
            Endian::Big => u16::from_be_bytes(arr),
        }
    })
}

fn decode_i16_iter(raw: &[u8], endian: Endian) -> impl Iterator<Item = i16> + '_ {
    raw.chunks_exact(2).map(move |c| {
        let arr = [c[0], c[1]];
        match endian {
            Endian::Little => i16::from_le_bytes(arr),
            Endian::Big => i16::from_be_bytes(arr),
        }
    })
}

fn decode_u32_iter(raw: &[u8], endian: Endian) -> impl Iterator<Item = u32> + '_ {
    raw.chunks_exact(4).map(move |c| {
        let arr = [c[0], c[1], c[2], c[3]];
        match endian {
            Endian::Little => u32::from_le_bytes(arr),
            Endian::Big => u32::from_be_bytes(arr),
        }
    })
}

fn decode_i32_iter(raw: &[u8], endian: Endian) -> impl Iterator<Item = i32> + '_ {
    raw.chunks_exact(4).map(move |c| {
        let arr = [c[0], c[1], c[2], c[3]];
        match endian {
            Endian::Little => i32::from_le_bytes(arr),
            Endian::Big => i32::from_be_bytes(arr),
        }
    })
}

fn decode_u64_iter(raw: &[u8], endian: Endian) -> impl Iterator<Item = u64> + '_ {
    raw.chunks_exact(8).map(move |c| {
        let arr = [c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]];
        match endian {
            Endian::Little => u64::from_le_bytes(arr),
            Endian::Big => u64::from_be_bytes(arr),
        }
    })
}

fn decode_i64_iter(raw: &[u8], endian: Endian) -> impl Iterator<Item = i64> + '_ {
    raw.chunks_exact(8).map(move |c| {
        let arr = [c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]];
        match endian {
            Endian::Little => i64::from_le_bytes(arr),
            Endian::Big => i64::from_be_bytes(arr),
        }
    })
}

fn decode_f32_iter(raw: &[u8], endian: Endian) -> impl Iterator<Item = f32> + '_ {
    raw.chunks_exact(4).map(move |c| {
        let arr = [c[0], c[1], c[2], c[3]];
        match endian {
            Endian::Little => f32::from_le_bytes(arr),
            Endian::Big => f32::from_be_bytes(arr),
        }
    })
}

fn decode_f64_iter(raw: &[u8], endian: Endian) -> impl Iterator<Item = f64> + '_ {
    raw.chunks_exact(8).map(move |c| {
        let arr = [c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]];
        match endian {
            Endian::Little => f64::from_le_bytes(arr),
            Endian::Big => f64::from_be_bytes(arr),
        }
    })
}

/// Drive the decoded element iterator and pick the right enum shape:
/// - all elements invalid → `RawValue::Invalid`
/// - exactly one element → `scalar(value)`
/// - many elements → `array(Box<[T]>)`
fn collapse<T, I, F, S, A>(iter: I, is_invalid: F, scalar: S, array: A) -> RawValue
where
    T: Copy,
    I: Iterator<Item = T>,
    F: Fn(&T) -> bool,
    S: FnOnce(T) -> RawValue,
    A: FnOnce(Box<[T]>) -> RawValue,
{
    let collected: Vec<T> = iter.collect();
    if collected.is_empty() || collected.iter().all(is_invalid) {
        return RawValue::Invalid;
    }
    if collected.len() == 1 {
        scalar(collected[0])
    } else {
        array(collected.into_boxed_slice())
    }
}

fn decode_byte(raw: &[u8]) -> RawValue {
    if !raw.is_empty() && raw.iter().all(|&b| b == 0xFF) {
        RawValue::Invalid
    } else {
        RawValue::Byte(raw.to_vec().into_boxed_slice())
    }
}

fn decode_string(raw: &[u8]) -> RawValue {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    if end == 0 {
        return RawValue::Invalid;
    }
    let s = String::from_utf8_lossy(&raw[..end]).into_owned();
    RawValue::String(s.into_boxed_str())
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
        assert_eq!(
            dec(BaseType::Enum, &[1, 0xFF, 4], Endian::Little),
            RawValue::EnumArray(vec![1u8, 0xFF, 4].into_boxed_slice())
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
            RawValue::U8zScalar(0xFF)
        );
    }

    #[test]
    fn byte_invalid_only_when_all_ff() {
        assert_eq!(
            dec(BaseType::Byte, &[0xFF], Endian::Little),
            RawValue::Invalid
        );
        assert_eq!(
            dec(BaseType::Byte, &[0xFF, 0x01, 0xFF], Endian::Little),
            RawValue::Byte(vec![0xFFu8, 0x01, 0xFF].into_boxed_slice())
        );
        assert_eq!(
            dec(BaseType::Byte, &[0xFF, 0xFF, 0xFF], Endian::Little),
            RawValue::Invalid
        );
    }

    #[test]
    fn uint16_endianness_and_invalid() {
        assert_eq!(
            dec(BaseType::UInt16, &[0x34, 0x12], Endian::Little),
            RawValue::U16Scalar(0x1234)
        );
        assert_eq!(
            dec(BaseType::UInt16, &[0x12, 0x34], Endian::Big),
            RawValue::U16Scalar(0x1234)
        );
        assert_eq!(
            dec(BaseType::UInt16, &[0xFF, 0xFF], Endian::Little),
            RawValue::Invalid
        );
    }

    #[test]
    fn uint32_le_decodes_known_timestamp() {
        assert_eq!(
            dec(BaseType::UInt32, &[0xF8, 0xEF, 0x59, 0x3B], Endian::Little),
            RawValue::U32Scalar(995749880)
        );
    }

    #[test]
    fn float32_invalid_is_all_ones_bit_pattern() {
        assert_eq!(
            dec(BaseType::Float32, &[0xFF, 0xFF, 0xFF, 0xFF], Endian::Little),
            RawValue::Invalid
        );
        let v = dec(BaseType::Float32, &[0x00, 0x00, 0x80, 0x3F], Endian::Little);
        match v {
            RawValue::F32Scalar(x) => assert_eq!(x, 1.0),
            _ => panic!("expected F32Scalar"),
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
        let v = RawValue::U32Scalar(995749880);
        assert_eq!(v.as_u32(), Some(995749880));
        assert!(!v.is_invalid());

        let v = RawValue::EnumScalar(4);
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
            RawValue::I8Scalar(126)
        );
    }

    #[test]
    fn sint16_array_partial_invalid_keeps_field() {
        assert_eq!(
            dec(BaseType::SInt16, &[0xFF, 0x7F, 0x05, 0x00], Endian::Little),
            RawValue::I16Array(vec![i16::MAX, 5].into_boxed_slice())
        );
    }
}
