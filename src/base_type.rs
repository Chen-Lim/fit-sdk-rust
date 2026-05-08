//! FIT base types — the 17 primitive wire types referenced by every Definition
//! message's field-type byte.
//!
//! The field-type byte layout is:
//!
//! ```text
//!   Bit 7    Bits [6:5]   Bits [4:0]
//!   ┌─────┬─────────────┬──────────────┐
//!   │ EE  │ reserved=00 │  type code   │
//!   └─────┴─────────────┴──────────────┘
//! ```
//!
//! - **EE**: endian flag — set when multi-byte values are big-endian. In
//!   practice the architecture byte (offset 2 of the Definition) is the
//!   source of truth for endianness; `EE` is largely redundant and we don't
//!   honor it at runtime.
//! - **type code**: one of 17 values (`0x00..=0x10`).
//!
//! Reference: `guide/fit_binary_learning_notes.md` §3.1.

use crate::error::FitError;

/// The 17 FIT base types, indexed by their type-code byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BaseType {
    /// `0x00` — 1-byte enumeration. Invalid: `0xFF`.
    Enum = 0x00,
    /// `0x01` — signed 8-bit. Invalid: `0x7F`.
    SInt8 = 0x01,
    /// `0x02` — unsigned 8-bit. Invalid: `0xFF`.
    UInt8 = 0x02,
    /// `0x03` — signed 16-bit. Invalid: `0x7FFF`.
    SInt16 = 0x03,
    /// `0x04` — unsigned 16-bit. Invalid: `0xFFFF`.
    UInt16 = 0x04,
    /// `0x05` — signed 32-bit. Invalid: `0x7FFFFFFF`.
    SInt32 = 0x05,
    /// `0x06` — unsigned 32-bit. Invalid: `0xFFFFFFFF`.
    UInt32 = 0x06,
    /// `0x07` — null-terminated UTF-8. Invalid: empty / first byte `0x00`.
    String = 0x07,
    /// `0x08` — IEEE 754 single. Invalid: bit-pattern `0xFFFFFFFF`.
    Float32 = 0x08,
    /// `0x09` — IEEE 754 double. Invalid: bit-pattern `0xFFFFFFFFFFFFFFFF`.
    Float64 = 0x09,
    /// `0x0A` — unsigned 8-bit (Z series). Invalid: **`0x00`**.
    UInt8z = 0x0A,
    /// `0x0B` — unsigned 16-bit (Z series). Invalid: **`0x0000`**.
    UInt16z = 0x0B,
    /// `0x0C` — unsigned 32-bit (Z series). Invalid: **`0x00000000`**.
    UInt32z = 0x0C,
    /// `0x0D` — opaque byte array. Invalid: **all** elements `0xFF` (special
    /// rule: a single `0xFF` element is *not* invalid in a multi-byte array).
    Byte = 0x0D,
    /// `0x0E` — signed 64-bit. Invalid: `0x7FFFFFFFFFFFFFFF`.
    SInt64 = 0x0E,
    /// `0x0F` — unsigned 64-bit. Invalid: `0xFFFFFFFFFFFFFFFF`.
    UInt64 = 0x0F,
    /// `0x10` — unsigned 64-bit (Z series). Invalid: **`0x0000000000000000`**.
    ///
    /// The 17th and largest valid type code; missing from many older
    /// references but defined in current FIT SDKs.
    UInt64z = 0x10,
}

impl BaseType {
    /// Mask to extract the type code from a field-type byte (drops endian flag).
    pub const TYPE_CODE_MASK: u8 = 0x1F;
    /// Endian flag bit (bit 7 of a field-type byte).
    pub const ENDIAN_FLAG: u8 = 0x80;

    /// Decode a raw field-type byte.
    ///
    /// Bit 7 (the endian flag) is masked off; only the low 5 bits are
    /// consulted. Returns [`FitError::UnknownBaseType`] if the type code
    /// is outside the valid range `0x00..=0x10`.
    pub fn from_byte(byte: u8) -> Result<Self, FitError> {
        let code = byte & Self::TYPE_CODE_MASK;
        let bt = match code {
            0x00 => Self::Enum,
            0x01 => Self::SInt8,
            0x02 => Self::UInt8,
            0x03 => Self::SInt16,
            0x04 => Self::UInt16,
            0x05 => Self::SInt32,
            0x06 => Self::UInt32,
            0x07 => Self::String,
            0x08 => Self::Float32,
            0x09 => Self::Float64,
            0x0A => Self::UInt8z,
            0x0B => Self::UInt16z,
            0x0C => Self::UInt32z,
            0x0D => Self::Byte,
            0x0E => Self::SInt64,
            0x0F => Self::UInt64,
            0x10 => Self::UInt64z,
            _ => return Err(FitError::UnknownBaseType(byte, code)),
        };
        Ok(bt)
    }

    /// True iff bit 7 of the raw field-type byte indicates big-endian.
    pub fn endian_flag_set(byte: u8) -> bool {
        byte & Self::ENDIAN_FLAG != 0
    }

    /// Size in bytes of a **single element** of this type. For the variable-
    /// length types ([`BaseType::String`] and [`BaseType::Byte`]) this is the
    /// stride per element (1 byte); the actual payload size comes from the
    /// Field Size byte in the Definition message.
    pub fn element_size(&self) -> usize {
        match self {
            Self::Enum | Self::SInt8 | Self::UInt8 | Self::UInt8z | Self::Byte | Self::String => 1,
            Self::SInt16 | Self::UInt16 | Self::UInt16z => 2,
            Self::SInt32 | Self::UInt32 | Self::UInt32z | Self::Float32 => 4,
            Self::SInt64 | Self::UInt64 | Self::UInt64z | Self::Float64 => 8,
        }
    }

    /// True for the Z series — types whose invalid sentinel is **all zero**
    /// rather than all ones. Important for invalid-value detection in M4.
    pub fn is_z_type(&self) -> bool {
        matches!(self, Self::UInt8z | Self::UInt16z | Self::UInt32z | Self::UInt64z)
    }

    /// True for [`BaseType::Byte`]. Required because the Byte type has a
    /// special invalid-value rule (only invalid when *every* element is
    /// `0xFF`).
    pub fn is_byte(&self) -> bool {
        matches!(self, Self::Byte)
    }

    /// True for [`BaseType::String`].
    pub fn is_string(&self) -> bool {
        matches!(self, Self::String)
    }

    /// Raw type code byte (0x00..=0x10). Since BaseType is `#[repr(u8)]`,
    /// this is the discriminant value — suitable for writing in a Definition
    /// message's field definition.
    pub fn type_code(&self) -> u8 {
        *self as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_17_codes_round_trip() {
        for code in 0x00..=0x10 {
            let bt = BaseType::from_byte(code).expect("code in range must decode");
            assert_eq!(bt as u8, code, "round-trip for 0x{code:02X}");
        }
    }

    #[test]
    fn endian_flag_is_masked() {
        // Same type code, endian flag set vs unset must yield the same BaseType.
        let with_flag = BaseType::from_byte(0x80 | 0x04).unwrap();
        let without = BaseType::from_byte(0x04).unwrap();
        assert_eq!(with_flag, BaseType::UInt16);
        assert_eq!(without, BaseType::UInt16);
        assert!(BaseType::endian_flag_set(0x80 | 0x04));
        assert!(!BaseType::endian_flag_set(0x04));
    }

    #[test]
    fn invalid_type_code_returns_error() {
        // 0x11..0x1F are reserved/invalid; 0x1F is the largest masked value.
        for bad in 0x11..=0x1F {
            assert!(matches!(BaseType::from_byte(bad), Err(FitError::UnknownBaseType(_, _))));
        }
    }

    #[test]
    fn element_sizes_are_correct() {
        assert_eq!(BaseType::Enum.element_size(), 1);
        assert_eq!(BaseType::UInt16.element_size(), 2);
        assert_eq!(BaseType::UInt32.element_size(), 4);
        assert_eq!(BaseType::Float32.element_size(), 4);
        assert_eq!(BaseType::UInt64.element_size(), 8);
        assert_eq!(BaseType::UInt64z.element_size(), 8);
        assert_eq!(BaseType::String.element_size(), 1);
        assert_eq!(BaseType::Byte.element_size(), 1);
    }

    #[test]
    fn z_type_classification() {
        assert!(BaseType::UInt8z.is_z_type());
        assert!(BaseType::UInt16z.is_z_type());
        assert!(BaseType::UInt32z.is_z_type());
        assert!(BaseType::UInt64z.is_z_type());
        assert!(!BaseType::UInt8.is_z_type());
        assert!(!BaseType::Byte.is_z_type());
    }

    #[test]
    fn uint64z_is_present_at_0x10() {
        // Regression check for the gap originally identified in the learning notes.
        assert_eq!(BaseType::from_byte(0x10).unwrap(), BaseType::UInt64z);
    }
}
