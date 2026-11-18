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

/// Unpack a parent field's wire bytes into [`UnpackedComponent`]s. Returns
/// an empty vec if `components` is empty, and stops once fewer bits remain
/// in `wire_bytes` than the next component declares (e.g. variable-length
/// `hr.event_timestamp_12`).
///
/// `wire_bytes` should be in least-significant-byte-first element order (matching
/// little-endian wire encoding of each numeric element).
pub fn unpack_bytes(components: &'static [Component], wire_bytes: &[u8]) -> Vec<UnpackedComponent> {
    if components.is_empty() {
        return Vec::new();
    }
    let mut bs = BitStream::new(wire_bytes);
    let mut out = Vec::with_capacity(components.len());
    for c in components {
        if bs.bits_remaining() < c.bits as u64 {
            break;
        }
        out.push(UnpackedComponent {
            target_name: c.name,
            raw: bs.read_bits(c.bits as u32),
            bits: c.bits,
            scale: c.scale,
            offset: c.offset,
            units: c.units,
            accumulate: c.accumulate,
        });
    }
    out
}

/// Materialise a single u64 (e.g. from a numeric `RawValue`) into LE bytes
/// big enough to hold all the component bits, then unpack it. Convenience
/// wrapper for fields whose [`RawValue`] is a length-1 numeric.
pub fn unpack_scalar(components: &'static [Component], scalar: u64) -> Vec<UnpackedComponent> {
    let total_bits: u32 = components.iter().map(|c| c.bits as u32).sum();
    let nbytes = (total_bits.div_ceil(8).max(1) as usize).min(8);
    let bytes = scalar.to_le_bytes();
    unpack_bytes(components, &bytes[..nbytes])
}

/// Unpack any numeric scalar, numeric array, or `Byte` [`RawValue`] into
/// components by encoding elements in little-endian byte order. Returns empty
/// for [`RawValue::Invalid`] or [`RawValue::String`].
pub fn unpack_raw(components: &'static [Component], raw: &RawValue) -> Vec<UnpackedComponent> {
    if components.is_empty() {
        return Vec::new();
    }
    let Some(bytes) = raw_to_le_bytes(raw) else {
        return Vec::new();
    };
    unpack_bytes(components, &bytes)
}

fn raw_to_le_bytes(raw: &RawValue) -> Option<Vec<u8>> {
    use RawValue::*;
    let bytes = match raw {
        Invalid | String(_) => return None,
        Byte(b) => b.to_vec(),
        EnumScalar(v) | U8Scalar(v) | U8zScalar(v) => vec![*v],
        I8Scalar(v) => vec![*v as u8],
        U16Scalar(v) | U16zScalar(v) => v.to_le_bytes().to_vec(),
        I16Scalar(v) => v.to_le_bytes().to_vec(),
        U32Scalar(v) | U32zScalar(v) => v.to_le_bytes().to_vec(),
        I32Scalar(v) => v.to_le_bytes().to_vec(),
        U64Scalar(v) | U64zScalar(v) => v.to_le_bytes().to_vec(),
        I64Scalar(v) => v.to_le_bytes().to_vec(),
        F32Scalar(v) => v.to_le_bytes().to_vec(),
        F64Scalar(v) => v.to_le_bytes().to_vec(),
        EnumArray(a) | U8Array(a) | U8zArray(a) => a.to_vec(),
        I8Array(a) => a.iter().map(|&x| x as u8).collect(),
        U16Array(a) | U16zArray(a) => a.iter().flat_map(|x| x.to_le_bytes()).collect(),
        I16Array(a) => a.iter().flat_map(|x| x.to_le_bytes()).collect(),
        U32Array(a) | U32zArray(a) => a.iter().flat_map(|x| x.to_le_bytes()).collect(),
        I32Array(a) => a.iter().flat_map(|x| x.to_le_bytes()).collect(),
        U64Array(a) | U64zArray(a) => a.iter().flat_map(|x| x.to_le_bytes()).collect(),
        I64Array(a) => a.iter().flat_map(|x| x.to_le_bytes()).collect(),
        F32Array(a) => a.iter().flat_map(|x| x.to_le_bytes()).collect(),
        F64Array(a) => a.iter().flat_map(|x| x.to_le_bytes()).collect(),
    };
    Some(bytes)
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
        assert_eq!(
            scalar_as_u64(&RawValue::U32Scalar(995749880)),
            Some(995749880)
        );
        assert_eq!(scalar_as_u64(&RawValue::Invalid), None);
        assert_eq!(scalar_as_u64(&RawValue::String("foo".into())), None);
        // Multi-element arrays are not scalars.
        assert_eq!(
            scalar_as_u64(&RawValue::U8Array(vec![1u8, 2].into_boxed_slice())),
            None
        );
    }

    #[test]
    fn unpack_bytes_stops_when_remaining_bits_shorter_than_component() {
        // 3 bytes = 24 bits; ask for three 12-bit components -> only 2 should unpack.
        static THREE_TWELVE_BIT: &[Component] = &[
            Component {
                name: "event_timestamp",
                bits: 12,
                scale: Some(1024.0),
                offset: None,
                units: Some("s"),
                accumulate: true,
            },
            Component {
                name: "event_timestamp",
                bits: 12,
                scale: Some(1024.0),
                offset: None,
                units: Some("s"),
                accumulate: true,
            },
            Component {
                name: "event_timestamp",
                bits: 12,
                scale: Some(1024.0),
                offset: None,
                units: Some("s"),
                accumulate: true,
            },
        ];
        let unpacked = unpack_bytes(THREE_TWELVE_BIT, &[0xBC, 0x3A, 0x12]);
        assert_eq!(unpacked.len(), 2);
        assert_eq!(unpacked[0].raw, 0xABC);
        assert_eq!(unpacked[1].raw, 0x123);
    }

    #[test]
    fn unpack_raw_handles_bytes_arrays_scalars_and_non_numeric() {
        let unpacked_u32 = unpack_raw(GEAR_CHANGE_COMPONENTS, &RawValue::U32Scalar(0x01_02_03_04));
        assert_eq!(unpacked_u32.len(), 4);
        assert_eq!(unpacked_u32[0].raw, 4);
        assert_eq!(unpacked_u32[3].raw, 1);

        let unpacked_u16_arr = unpack_raw(
            GEAR_CHANGE_COMPONENTS,
            &RawValue::U16Array(vec![0x0304, 0x0102].into_boxed_slice()),
        );
        assert_eq!(unpacked_u16_arr.len(), 4);
        assert_eq!(unpacked_u16_arr[0].raw, 4);
        assert_eq!(unpacked_u16_arr[1].raw, 3);
        assert_eq!(unpacked_u16_arr[2].raw, 2);
        assert_eq!(unpacked_u16_arr[3].raw, 1);

        let unpacked_bytes = unpack_raw(
            GEAR_CHANGE_COMPONENTS,
            &RawValue::Byte(vec![4, 3, 2, 1].into_boxed_slice()),
        );
        assert_eq!(unpacked_bytes.len(), 4);
        assert_eq!(unpacked_bytes[2].raw, 2);

        assert!(unpack_raw(GEAR_CHANGE_COMPONENTS, &RawValue::Invalid).is_empty());
        assert!(unpack_raw(GEAR_CHANGE_COMPONENTS, &RawValue::String("abc".into())).is_empty());
    }
}
