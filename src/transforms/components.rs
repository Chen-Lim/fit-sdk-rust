//! Components — split a packed integer field into multiple LSB-first lanes.
//!
//! When a [`crate::profile::FieldInfo`] has a non-empty `components` slice,
//! the parent field's wire bytes are interpreted as a packed integer that
//! unpacks bit-for-bit into the named sub-fields. Each component's `bits`
//! value defines its width; reading proceeds least-significant bit first.
//!
//! Reference: `guide/fit_binary_learning_notes.md` §"补充知识：BitStream".

use crate::profile::Component;
use crate::raw_value::RawValue;
use crate::transforms::bit_stream::BitStream;

/// Result of unpacking one component.
#[derive(Debug, Clone, PartialEq)]
pub struct UnpackedComponent {
    /// Component's name from Profile.xlsx — points at another field on the
    /// same message that the value should be stored under.
    pub target_name: &'static str,
    /// Raw integer extracted from the bit stream. Apply scale/offset
    /// downstream if needed.
    pub raw: u64,
    pub bits: u8,
    pub scale: Option<f64>,
    pub offset: Option<f64>,
    pub units: Option<&'static str>,
    pub accumulate: bool,
}

/// Unpack a parent field's wire bytes into one [`UnpackedComponent`] per
/// declared component. Returns an empty vec if `components` is empty.
///
/// `wire_bytes` should be the raw bytes of the parent field in the order
/// they appeared in the file (the decoder handles endianness when filling
/// `RawValue`, but Components specifically read **bit-by-bit LSB-first
/// from the original byte sequence** — independent of the parent's logical
/// endianness, because the operation is on a contiguous bit string).
pub fn unpack_bytes(components: &'static [Component], wire_bytes: &[u8]) -> Vec<UnpackedComponent> {
    if components.is_empty() {
        return Vec::new();
    }
    let mut bs = BitStream::new(wire_bytes);
    components
        .iter()
        .map(|c| UnpackedComponent {
            target_name: c.name,
            raw: bs.read_bits(c.bits as u32),
            bits: c.bits,
            scale: c.scale,
            offset: c.offset,
            units: c.units,
            accumulate: c.accumulate,
        })
        .collect()
}

/// Materialise a single u64 (e.g. from a numeric `RawValue`) into LE bytes
/// big enough to hold all the component bits, then unpack it. Convenience
/// wrapper for fields whose [`RawValue`] is a length-1 numeric.
pub fn unpack_scalar(components: &'static [Component], scalar: u64) -> Vec<UnpackedComponent> {
    let total_bits: u32 = components.iter().map(|c| c.bits as u32).sum();
    let nbytes = total_bits.div_ceil(8).max(1) as usize;
    let bytes: Vec<u8> = (0..nbytes)
        .map(|i| ((scalar >> (i * 8)) & 0xFF) as u8)
        .collect();
    unpack_bytes(components, &bytes)
}

/// Best-effort conversion of a numeric [`RawValue`] to a u64. Returns `None`
/// for non-numeric variants (`String`, `Bytes`, `Invalid`).
#[inline]
pub fn scalar_as_u64(raw: &RawValue) -> Option<u64> {
    raw.scalar_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mimic the four 8-bit lanes of `gear_change_data`.
    static GEAR_CHANGE_COMPONENTS: &[Component] = &[
        Component {
            name: "rear_gear",
            bits: 8,
            scale: None,
            offset: None,
            units: None,
            accumulate: false,
        },
        Component {
            name: "rear_gear_num",
            bits: 8,
            scale: None,
            offset: None,
            units: None,
            accumulate: false,
        },
        Component {
            name: "front_gear",
            bits: 8,
            scale: None,
            offset: None,
            units: None,
            accumulate: false,
        },
        Component {
            name: "front_gear_num",
            bits: 8,
            scale: None,
            offset: None,
            units: None,
            accumulate: false,
        },
    ];

    #[test]
    fn unpacks_gear_change_data_from_bytes() {
        let bytes = [0x04, 0x03, 0x02, 0x01]; // LSB → rear=4, rear_num=3, front=2, front_num=1
        let unpacked = unpack_bytes(GEAR_CHANGE_COMPONENTS, &bytes);
        assert_eq!(unpacked.len(), 4);
        assert_eq!(unpacked[0].target_name, "rear_gear");
        assert_eq!(unpacked[0].raw, 4);
        assert_eq!(unpacked[1].raw, 3);
        assert_eq!(unpacked[2].raw, 2);
        assert_eq!(unpacked[3].raw, 1);
    }

    #[test]
    fn unpacks_gear_change_from_scalar() {
        // Same value as a packed u32: 0x01020304 (front_num=1, front=2, rear_num=3, rear=4)
        let scalar = 0x01_02_03_04u64;
        let unpacked = unpack_scalar(GEAR_CHANGE_COMPONENTS, scalar);
        assert_eq!(unpacked[0].raw, 4);
        assert_eq!(unpacked[3].raw, 1);
    }

    #[test]
    fn empty_components_yields_empty() {
        let unpacked = unpack_bytes(&[], &[1, 2, 3, 4]);
        assert!(unpacked.is_empty());
    }

    #[test]
    fn scalar_as_u64_handles_common_cases() {
        assert_eq!(scalar_as_u64(&RawValue::U8Scalar(42)), Some(42));
        assert_eq!(scalar_as_u64(&RawValue::U16Scalar(1234)), Some(1234));
        assert_eq!(scalar_as_u64(&RawValue::U32Scalar(995749880)), Some(995749880));
        assert_eq!(scalar_as_u64(&RawValue::Invalid), None);
        assert_eq!(scalar_as_u64(&RawValue::String("foo".into())), None);
        // Multi-element arrays are not scalars.
        assert_eq!(
            scalar_as_u64(&RawValue::U8Array(vec![1u8, 2].into_boxed_slice())),
            None
        );
    }
}
